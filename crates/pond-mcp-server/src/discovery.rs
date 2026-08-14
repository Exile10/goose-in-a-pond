//! Discovery MCP Server — country data, food products, and prices.
//!
//! Provides 3 tools: `get_country_info` (REST Countries), `lookup_product` (Open Food Facts),
//! `get_product_price` (Open Prices). All APIs are free and require no API keys.
//!
//! A fourth, `search_web`, is present in the file but **not registered** — see the
//! note on it. `SettingsRepository` is still a dependency because that is the only
//! thing that reads `searxng_url`, and it comes back with the tool.

use pond_core::user_data::ports::settings::SettingsRepository;
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
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CountryInfoParams {
    /// Country name or uppercase 2-3 letter code.
    pub country: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ProductLookupParams {
    /// EAN/UPC barcode digits.
    pub barcode: Option<String>,
    pub name: Option<String>,
    /// Default 3, max 5.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ProductPriceParams {
    /// Product barcode (names are rejected).
    pub product: Option<String>,
    pub location: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WebSearchParams {
    pub query: Option<String>,
    /// Default 5, max 10.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Constants ──────────────────────────────────────────────────────────────

const REST_COUNTRIES_BASE: &str = "https://restcountries.com/v3.1";
const OFF_BASE: &str = "https://world.openfoodfacts.org";
const OFF_PRICES_BASE: &str = "https://prices.openfoodfacts.org/api/v1";

const COUNTRY_INFO_BUDGET: usize = 800;
const PRODUCT_SINGLE_BUDGET: usize = 1000;
const PRODUCT_SEARCH_BUDGET: usize = 1500;
const PRODUCT_PRICE_BUDGET: usize = 800;
const WEB_SEARCH_BUDGET: usize = 1500;

const COUNTRY_FIELDS: &str = "name,capital,population,currencies,languages,flags,region,subregion";

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DiscoveryMcpServer {
    http_client: reqwest::Client,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DiscoveryMcpServer {
    pub fn new(
        http_client: reqwest::Client,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self {
            http_client,
            settings_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Look up country data: population, capital, currency, languages, region.")]
    async fn get_country_info(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<CountryInfoParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_country_info");
        let country = resolve_country(&params.0).await;
        eprintln!("[discovery] get_country_info: country={:?}", country);

        if country.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a country name or code. Retry with a 'country' parameter \
                 (e.g. 'Kenya', 'US', 'GBR').",
            )]));
        }

        // Choose endpoint: alpha for 2-3 letter all-caps codes, name for everything else
        let url = if looks_like_country_code(&country) {
            format!(
                "{}/alpha/{}?fields={}",
                REST_COUNTRIES_BASE,
                urlencoding::encode(&country),
                COUNTRY_FIELDS,
            )
        } else {
            format!(
                "{}/name/{}?fields={}",
                REST_COUNTRIES_BASE,
                urlencoding::encode(&country),
                COUNTRY_FIELDS,
            )
        };
        eprintln!("[discovery] GET {}", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[discovery] REST Countries request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("REST Countries", &e.to_string()),
                )]));
            }
        };

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            eprintln!("[discovery] country '{}' not found", country);
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!("country data for '{}'", country),
                    &["giap-knowledge__get_wikipedia_article"],
                ),
            )]));
        }

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[discovery] REST Countries HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("REST Countries", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[discovery] failed to parse REST Countries response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("REST Countries", &e.to_string()),
                )]));
            }
        };

        // REST Countries returns an array for /name, single object for /alpha
        let entry = if body.is_array() {
            body.as_array().and_then(|arr| arr.first())
        } else {
            Some(&body)
        };

        let text = match entry {
            Some(c) => format_country(c),
            None => crate::format::format_no_results(
                &format!("country data for '{}'", country),
                &["giap-knowledge__get_wikipedia_article"],
            ),
        };

        let truncated = crate::format::truncate_to_budget(&text, COUNTRY_INFO_BUDGET);
        eprintln!(
            "[discovery] get_country_info done, {} chars",
            truncated.len()
        );
        Ok(CallToolResult::success(vec![Content::text(truncated)]))
    }

    #[tool(description = "\
Look up a food product by barcode or name: nutrition, ingredients, allergens, Nutri-Score.")]
    async fn lookup_product(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ProductLookupParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("lookup_product");
        let (barcode, name) = resolve_product(&params.0).await;
        eprintln!(
            "[discovery] lookup_product: barcode={:?}, name={:?}",
            barcode, name
        );

        if barcode.is_none() && name.is_none() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a product barcode or name. Retry with a 'barcode' parameter \
                 (e.g. '3017620422003') or 'name' parameter (e.g. 'nutella').",
            )]));
        }

        // Barcode lookup — direct product fetch
        if let Some(ref code) = barcode {
            let url = format!("{}/api/v2/product/{}", OFF_BASE, urlencoding::encode(code));
            eprintln!("[discovery] GET {}", url);

            let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
                b.timeout(std::time::Duration::from_secs(10))
            })
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[discovery] OFF barcode request failed: {e}");
                    return Ok(CallToolResult::success(vec![Content::text(
                        crate::format::format_api_error("Open Food Facts", &e.to_string()),
                    )]));
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                eprintln!("[discovery] OFF barcode HTTP {status}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Food Facts", &format!("HTTP {status}")),
                )]));
            }

            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[discovery] failed to parse OFF barcode response: {e}");
                    return Ok(CallToolResult::success(vec![Content::text(
                        crate::format::format_api_error("Open Food Facts", &e.to_string()),
                    )]));
                }
            };

            let status = body["status"].as_u64().unwrap_or(0);
            if status == 0 {
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_no_results(
                        // A barcode is not something any other tool here can look up.
                        &format!("barcode '{}' in the Open Food Facts database", code),
                        &[],
                    ),
                )]));
            }

            let product_data = &body["product"];
            let text = format_product(product_data);
            let truncated = crate::format::truncate_to_budget(&text, PRODUCT_SINGLE_BUDGET);
            eprintln!(
                "[discovery] lookup_product (barcode) done, {} chars",
                truncated.len()
            );

            let ui_data = build_product_ui_data(product_data);
            let hint = format!("[[[mcp-ui:product:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, truncated);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        // Name search
        let query = name.as_deref().unwrap_or("");
        let limit = params.0.limit.unwrap_or(3).clamp(1, 5);
        let url = format!(
            "{}/cgi/search.pl?search_terms={}&json=1&page_size={}",
            OFF_BASE,
            urlencoding::encode(query),
            limit,
        );
        eprintln!("[discovery] GET {}", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(15))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[discovery] OFF search request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Food Facts", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[discovery] OFF search HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Open Food Facts", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[discovery] failed to parse OFF search response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Food Facts", &e.to_string()),
                )]));
            }
        };

        let products = body["products"].as_array();
        let items: Vec<String> = products
            .map(|arr| arr.iter().map(format_product).collect())
            .unwrap_or_default();

        if items.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!("products matching '{}'", query),
                    &["giap-discovery__lookup_product"],
                ),
            )]));
        }

        // Build UI hint from first product in search results
        if let Some(arr) = products {
            if let Some(first) = arr.first() {
                let ui_data = build_product_ui_data(first);
                let header = format!("Products matching '{}':", query);
                let text =
                    crate::format::format_list_result(&items, &header, PRODUCT_SEARCH_BUDGET);
                eprintln!(
                    "[discovery] lookup_product (search) done, {} items, {} chars",
                    items.len(),
                    text.len()
                );
                let hint = format!("[[[mcp-ui:product:{}]]]\n", ui_data);
                let full_result = format!("{}{}", hint, text);
                return Ok(CallToolResult::success(vec![Content::text(full_result)]));
            }
        }

        let header = format!("Products matching '{}':", query);
        let text = crate::format::format_list_result(&items, &header, PRODUCT_SEARCH_BUDGET);
        eprintln!(
            "[discovery] lookup_product (search) done, {} items, {} chars",
            items.len(),
            text.len()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "\
Recent crowdsourced prices for a product barcode (find it via lookup_product). Coverage strongest in Europe.")]
    async fn get_product_price(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ProductPriceParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_product_price");
        let product = resolve_price_product(&params.0).await;
        eprintln!("[discovery] get_product_price: product={:?}", product);

        if product.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a product barcode or name. Retry with a 'product' parameter.",
            )]));
        }

        // Check if the input looks like a barcode
        if !input_is_barcode(&product) {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "The Open Prices API works best with barcodes. '{}' looks like a product name. \
                 Use lookup_product first to find the barcode, then call get_product_price with the barcode.",
                product,
            ))]));
        }

        let url = format!(
            "{}/prices?product_code={}&order_by=-date&page_size=5",
            OFF_PRICES_BASE,
            urlencoding::encode(&product),
        );
        eprintln!("[discovery] GET {}", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[discovery] Open Prices request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Prices", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[discovery] Open Prices HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Open Prices", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[discovery] failed to parse Open Prices response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Prices", &e.to_string()),
                )]));
            }
        };

        let items_arr = body["items"].as_array();
        let items: Vec<String> = items_arr
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let price = item["price"].as_f64()?;
                        let currency = item["currency"].as_str().unwrap_or("???");
                        let location = item["location"]["osm_name"]
                            .as_str()
                            .unwrap_or("Unknown location");
                        let date = item["date"].as_str().unwrap_or("unknown date");
                        Some(format!(
                            "{:.2} {} at {} ({})",
                            price, currency, location, date,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if items.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results("crowdsourced prices for this product", &[]),
            )]));
        }

        let product_name = body["items"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["product"]["product_name"].as_str())
            .unwrap_or("Product");

        let header = format!("**{}** recent prices:", product_name);
        let text = crate::format::format_list_result(&items, &header, PRODUCT_PRICE_BUDGET);
        eprintln!(
            "[discovery] get_product_price done, {} items, {} chars",
            items.len(),
            text.len()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// General web search. **DISABLED 2026-08-13 — not registered as a tool.**
    ///
    /// The `#[tool(...)]` attribute is deliberately absent, which is the whole
    /// of the disable: the router only collects annotated methods, so the model
    /// is never offered this and pays no schema for it. Restoring it is putting
    /// the attribute back:
    ///
    /// ```ignore
    /// #[tool(description = "\
    /// General web search. LAST RESORT — prefer specific tools (news, finance, knowledge) first.")]
    /// ```
    ///
    /// **Why it went.** SearXNG was the only backend left after the DuckDuckGo
    /// Instant Answer fallback was removed, and SearXNG is something the user
    /// has to run. So out of the box the tool could only ever return a dead end
    /// — while still costing a schema in every turn's prompt and still being
    /// picked by a model that reads "general web search" and believes it.
    ///
    /// The body is kept, and kept compiling, on purpose. A `#[cfg(feature)]`
    /// would have hidden it from the compiler, and AGENTS.md's standing warning
    /// applies: code CI only ever `check`s, or does not build at all, rots. This
    /// still type-checks against `SettingsRepository` and `format`, so whatever
    /// backend comes back — SearXNG, Brave, Mojeek — starts from working code.
    ///
    /// `search_searxng` below is reached only from here, so it is dead with it
    /// rather than separately.
    #[allow(dead_code)]
    async fn search_web(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WebSearchParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("search_web");
        let query = resolve_web_query(&params.0).await;
        eprintln!("[discovery] search_web: query={:?}", query);

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a search query. Retry with a 'query' parameter.",
            )]));
        }

        let limit = params.0.limit.unwrap_or(5).clamp(1, 10);

        // Check if SearXNG is configured
        let searxng_url = match self.settings_repo.get().await {
            Ok(s) => s.searxng_url.filter(|u| !u.trim().is_empty()),
            Err(e) => {
                eprintln!("[discovery] failed to load settings: {e}");
                None
            }
        };

        if let Some(ref base_url) = searxng_url {
            return self.search_searxng(base_url, &query, limit).await;
        }

        // No backend. SearXNG is the only one this tool has since the
        // DuckDuckGo Instant Answer fallback was removed, and DDG was never a
        // web search — it returned an abstract and a handful of related topics,
        // which is why its own header called itself "limited".
        //
        // Terminal on purpose, and the wording is load-bearing. search_web is
        // the last resort, so a miss here ends the chain and the model has to be
        // told so, or it repeats its previous sentence instead of reporting the
        // outcome. (Measured on gemma-4-E2B: an earlier settings-tip wording
        // produced a verbatim repeat of the preamble it had already streamed.)
        eprintln!("[discovery] search_web: no SearXNG configured, nothing to search");
        Ok(CallToolResult::success(vec![Content::text(
            crate::format::format_dead_end(
                &format!("web results for '{}'", query),
                "this pond has no web search backend configured, and search_web is \
                 the last resort, so there is nothing further to try. Tell the user \
                 plainly that you could not find it, and that setting a SearXNG \
                 instance in Settings would enable web search.",
            ),
        )]))
    }
}

impl DiscoveryMcpServer {
    /// Search via SearXNG instance.
    async fn search_searxng(
        &self,
        base_url: &str,
        query: &str,
        limit: u32,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let url = format!(
            "{}/search?q={}&format=json&categories=general&language=en&pageno=1",
            base_url.trim_end_matches('/'),
            urlencoding::encode(query),
        );
        eprintln!("[discovery] SearXNG GET {}", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[discovery] SearXNG request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("SearXNG", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[discovery] SearXNG HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("SearXNG", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[discovery] failed to parse SearXNG response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("SearXNG", &e.to_string()),
                )]));
            }
        };

        let results = body["results"].as_array();
        let items: Vec<String> = results
            .map(|arr| {
                arr.iter()
                    .take(limit as usize)
                    .filter_map(|item| {
                        let title = item["title"].as_str()?;
                        let url = item["url"].as_str().unwrap_or("");
                        let snippet = item["content"].as_str().unwrap_or("");
                        let domain = extract_domain(url);

                        let summary = if snippet.len() > 150 {
                            format!(
                                "{}...",
                                &snippet[..snippet
                                    .char_indices()
                                    .nth(150)
                                    .map(|(i, _)| i)
                                    .unwrap_or(snippet.len())]
                            )
                        } else {
                            snippet.to_string()
                        };

                        Some(format!("**{}** -- {} ({})", title, summary, domain))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if items.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!("web results for '{}'", query),
                    &[
                        "giap-knowledge__get_wikipedia_article",
                        "giap-knowledge__search_wikipedia",
                    ],
                ),
            )]));
        }

        // Build UI hint from SearXNG results
        let ui_results: Vec<serde_json::Value> = results
            .map(|arr| {
                arr.iter()
                    .take(limit as usize)
                    .filter_map(|item| {
                        let title = item["title"].as_str()?;
                        let url = item["url"].as_str().unwrap_or("");
                        let snippet = item["content"].as_str().unwrap_or("");
                        Some(serde_json::json!({
                            "title": title,
                            "snippet": snippet,
                            "url": url,
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let header = format!("Web search: \"{}\"", query);
        let text = crate::format::format_list_result(&items, &header, WEB_SEARCH_BUDGET);
        eprintln!(
            "[discovery] search_web (SearXNG) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_results.is_empty() {
            let ui_data = serde_json::json!({
                "query": query,
                "results": ui_results,
            });
            let hint = format!("[[[mcp-ui:search:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for DiscoveryMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-discovery",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Discovery server — products and countries.\n\n\
                 Tools: get_country_info (country data), lookup_product (food/nutrition), \
                 get_product_price (price lookup).\n\n\
                 For country questions: get_country_info. For food/nutrition: lookup_product.\n\
                 For product pricing: get_product_price (works best with barcodes).\n\
                 This server does not search the web. If the question needs a general \
                 web search, say so plainly rather than guessing.\n\
                 All tools are free, no API keys needed.",
            )
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Check if a string looks like a barcode (8-13 digits, all numeric).
pub fn input_is_barcode(s: &str) -> bool {
    let trimmed = s.trim();
    let len = trimmed.len();
    (8..=13).contains(&len) && trimmed.chars().all(|c| c.is_ascii_digit())
}

/// Check if a string looks like a country code (2-3 uppercase letters).
pub fn looks_like_country_code(s: &str) -> bool {
    let trimmed = s.trim();
    let len = trimmed.len();
    (2..=3).contains(&len) && trimmed.chars().all(|c| c.is_ascii_uppercase())
}

/// Format a population number with commas: 53771296 -> "53,771,296".
pub fn format_population(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Format a country entry from REST Countries API.
fn format_country(c: &serde_json::Value) -> String {
    let common_name = c["name"]["common"].as_str().unwrap_or("Unknown");
    let official_name = c["name"]["official"].as_str().unwrap_or("");
    let region = c["region"].as_str().unwrap_or("Unknown");
    let subregion = c["subregion"].as_str().unwrap_or("");
    let population = c["population"].as_u64().unwrap_or(0);
    let flag = c["flags"]["emoji"]
        .as_str()
        .or_else(|| c["flag"].as_str())
        .unwrap_or("");

    let capitals: Vec<&str> = c["capital"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let capital_str = if capitals.is_empty() {
        "N/A".to_string()
    } else {
        capitals.join(", ")
    };

    let currencies: Vec<String> = c["currencies"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(code, info)| {
                    let name = info["name"].as_str().unwrap_or(code);
                    format!("{} ({})", name, code)
                })
                .collect()
        })
        .unwrap_or_default();
    let currency_str = if currencies.is_empty() {
        "N/A".to_string()
    } else {
        currencies.join(", ")
    };

    let languages: Vec<&str> = c["languages"]
        .as_object()
        .map(|obj| obj.values().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let language_str = if languages.is_empty() {
        "N/A".to_string()
    } else {
        languages.join(", ")
    };

    let region_str = if subregion.is_empty() {
        region.to_string()
    } else {
        format!("{} ({})", region, subregion)
    };

    let name_line = if official_name.is_empty() || official_name == common_name {
        format!("**{}**", common_name)
    } else {
        format!("**{}** ({})", common_name, official_name)
    };

    format!(
        "{}\nRegion: {}\nCapital: {}\nPopulation: {}\nCurrencies: {}\nLanguages: {}\nFlag: {}",
        name_line,
        region_str,
        capital_str,
        format_population(population),
        currency_str,
        language_str,
        flag,
    )
}

/// Build structured UI data for a product card.
fn build_product_ui_data(p: &serde_json::Value) -> serde_json::Value {
    let name = p["product_name"].as_str().unwrap_or("Unknown product");
    let brands = p["brands"].as_str().unwrap_or("");
    let nutri_score = p["nutrition_grades"].as_str().unwrap_or("N/A");
    let calories = p["nutriments"]["energy-kcal_100g"]
        .as_f64()
        .map(|v| format!("{:.0} kcal/100g", v))
        .unwrap_or_else(|| "N/A".to_string());

    serde_json::json!({
        "name": name,
        "brand": brands,
        "nutri_score": nutri_score.to_uppercase(),
        "calories": calories,
    })
}

/// Format a single product from Open Food Facts.
fn format_product(p: &serde_json::Value) -> String {
    let name = p["product_name"].as_str().unwrap_or("Unknown product");
    let brands = p["brands"].as_str().unwrap_or("");
    let nutri_score = p["nutrition_grades"].as_str().unwrap_or("N/A");

    let calories = p["nutriments"]["energy-kcal_100g"]
        .as_f64()
        .map(|v| format!("{:.0} kcal/100g", v))
        .unwrap_or_else(|| "N/A".to_string());

    let ingredients = p["ingredients_text"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect::<String>();

    let allergens = p["allergens"]
        .as_str()
        .unwrap_or("")
        .replace("en:", "")
        .replace(',', ", ");

    let mut result = if brands.is_empty() {
        format!("**{}**", name)
    } else {
        format!("**{}** by {}", name, brands)
    };

    result.push_str(&format!(
        "\nNutri-Score: {} | Calories: {}",
        nutri_score.to_uppercase(),
        calories,
    ));

    if !ingredients.is_empty() {
        let display = if ingredients.len() >= 200 {
            format!("{}...", ingredients)
        } else {
            ingredients
        };
        result.push_str(&format!("\nIngredients: {}", display));
    }

    let trimmed_allergens = allergens.trim();
    if !trimmed_allergens.is_empty() {
        result.push_str(&format!("\nAllergens: {}", trimmed_allergens));
    }

    result
}

/// Extract domain from a URL for compact display.
fn extract_domain(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_string()
}

// ── Parameter resolution ──────────────────────────────────────────────────

const COUNTRY_SCHEMA: &str = r#"{"type":"object","properties":{"country":{"type":"string","description":"Country name or code to look up"}},"required":["country"]}"#;

async fn resolve_country(params: &CountryInfoParams) -> String {
    // 1. ToolCaller specialist
    if let Some(args) = crate::generate_params("get_country_info", COUNTRY_SCHEMA).await {
        if let Some(c) = args.get("country").and_then(|v| v.as_str()) {
            let trimmed = c.trim();
            if !trimmed.is_empty() {
                eprintln!(
                    "[discovery] resolve_country: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. Model params
    if let Some(ref c) = params.country {
        let trimmed = c.trim();
        if !trimmed.is_empty() {
            eprintln!(
                "[discovery] resolve_country: model param 'country': {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }

    // 3. Scan extras
    for key in &["name", "country", "nation", "code", "region"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    eprintln!(
                        "[discovery] resolve_country: extras '{}': {:?}",
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
        let cleaned = crate::clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            eprintln!(
                "[discovery] resolve_country: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    eprintln!("[discovery] resolve_country: no country found");
    String::new()
}

const PRODUCT_SCHEMA: &str = r#"{"type":"object","properties":{"barcode":{"type":"string","description":"Product barcode (EAN/UPC)"},"name":{"type":"string","description":"Product name to search"}}}"#;

/// Resolve product from params. Returns (barcode, name) — at most one will be Some.
async fn resolve_product(params: &ProductLookupParams) -> (Option<String>, Option<String>) {
    // 1. ToolCaller specialist
    if let Some(args) = crate::generate_params("lookup_product", PRODUCT_SCHEMA).await {
        if let Some(b) = args.get("barcode").and_then(|v| v.as_str()) {
            let trimmed = b.trim();
            if !trimmed.is_empty() && input_is_barcode(trimmed) {
                eprintln!(
                    "[discovery] resolve_product: ToolCaller barcode: {:?}",
                    trimmed
                );
                return (Some(trimmed.to_string()), None);
            }
        }
        if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
            let trimmed = n.trim();
            if !trimmed.is_empty() {
                eprintln!(
                    "[discovery] resolve_product: ToolCaller name: {:?}",
                    trimmed
                );
                return (None, Some(trimmed.to_string()));
            }
        }
    }

    // 2. Model params
    if let Some(ref b) = params.barcode {
        let trimmed = b.trim();
        if !trimmed.is_empty() {
            eprintln!(
                "[discovery] resolve_product: model param 'barcode': {:?}",
                trimmed
            );
            return (Some(trimmed.to_string()), None);
        }
    }
    if let Some(ref n) = params.name {
        let trimmed = n.trim();
        if !trimmed.is_empty() {
            eprintln!(
                "[discovery] resolve_product: model param 'name': {:?}",
                trimmed
            );
            return (None, Some(trimmed.to_string()));
        }
    }

    // 3. Scan extras
    for key in &["barcode", "ean", "upc"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    eprintln!(
                        "[discovery] resolve_product: extras '{}' (barcode): {:?}",
                        key, trimmed
                    );
                    return (Some(trimmed.to_string()), None);
                }
            }
        }
    }
    for key in &["product", "item", "food", "name", "query"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    // Check if it's actually a barcode
                    if input_is_barcode(trimmed) {
                        eprintln!(
                            "[discovery] resolve_product: extras '{}' (detected barcode): {:?}",
                            key, trimmed
                        );
                        return (Some(trimmed.to_string()), None);
                    }
                    eprintln!(
                        "[discovery] resolve_product: extras '{}' (name): {:?}",
                        key, trimmed
                    );
                    return (None, Some(trimmed.to_string()));
                }
            }
        }
    }

    // 4. Clean user message
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = crate::clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            if input_is_barcode(&cleaned) {
                eprintln!(
                    "[discovery] resolve_product: user message barcode: {:?}",
                    cleaned
                );
                return (Some(cleaned), None);
            }
            eprintln!(
                "[discovery] resolve_product: user message name: {:?}",
                cleaned
            );
            return (None, Some(cleaned));
        }
    }

    eprintln!("[discovery] resolve_product: no product found");
    (None, None)
}

const PRICE_SCHEMA: &str = r#"{"type":"object","properties":{"product":{"type":"string","description":"Product barcode or name"}},"required":["product"]}"#;

async fn resolve_price_product(params: &ProductPriceParams) -> String {
    // 1. ToolCaller specialist
    if let Some(args) = crate::generate_params("get_product_price", PRICE_SCHEMA).await {
        if let Some(p) = args.get("product").and_then(|v| v.as_str()) {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                eprintln!(
                    "[discovery] resolve_price_product: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. Model params
    if let Some(ref p) = params.product {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            eprintln!(
                "[discovery] resolve_price_product: model param 'product': {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }

    // 3. Scan extras
    for key in &["product", "item", "barcode", "price", "cost"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    eprintln!(
                        "[discovery] resolve_price_product: extras '{}': {:?}",
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
        let cleaned = crate::clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            eprintln!(
                "[discovery] resolve_price_product: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    eprintln!("[discovery] resolve_price_product: no product found");
    String::new()
}

const WEB_SEARCH_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"Web search query"}},"required":["query"]}"#;

async fn resolve_web_query(params: &WebSearchParams) -> String {
    // 1. ToolCaller specialist
    if let Some(args) = crate::generate_params("search_web", WEB_SEARCH_SCHEMA).await {
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            let trimmed = q.trim();
            if !trimmed.is_empty() {
                eprintln!(
                    "[discovery] resolve_web_query: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. Model params
    if let Some(ref q) = params.query {
        let trimmed = q.trim();
        if !trimmed.is_empty() {
            eprintln!(
                "[discovery] resolve_web_query: model param 'query': {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }

    // 3. Scan extras
    for key in &["query", "q", "search", "question"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    eprintln!(
                        "[discovery] resolve_web_query: extras '{}': {:?}",
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
        let cleaned = crate::clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            eprintln!(
                "[discovery] resolve_web_query: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    eprintln!("[discovery] resolve_web_query: no query found");
    String::new()
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DiscoveryDeps {
    http_client: reqwest::Client,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
}

static DISCOVERY_DEPS: OnceLock<DiscoveryDeps> = OnceLock::new();

/// Initialize discovery server dependencies. Call once at startup.
pub fn init_discovery_deps(
    http_client: reqwest::Client,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
) {
    let _ = DISCOVERY_DEPS.set(DiscoveryDeps {
        http_client,
        settings_repo,
    });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_discovery_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = DISCOVERY_DEPS
        .get()
        .expect("init_discovery_deps() not called");
    let server = DiscoveryMcpServer::new(deps.http_client.clone(), deps.settings_repo.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-discovery MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_barcode_detects_numeric_strings() {
        // EAN-13
        assert!(input_is_barcode("3017620422003"));
        // UPC-A (12 digits)
        assert!(input_is_barcode("012345678905"));
        // EAN-8
        assert!(input_is_barcode("12345678"));
        // Not a barcode — text
        assert!(!input_is_barcode("nutella"));
        // Too short
        assert!(!input_is_barcode("123"));
        // Too long
        assert!(!input_is_barcode("12345678901234"));
        // Mixed alphanumeric
        assert!(!input_is_barcode("30176ABC22003"));
        // Whitespace padded — still valid after trim
        assert!(input_is_barcode(" 3017620422003 "));
    }

    #[test]
    fn country_code_detection() {
        // 2-letter uppercase
        assert!(looks_like_country_code("US"));
        assert!(looks_like_country_code("KE"));
        assert!(looks_like_country_code("GB"));
        // 3-letter uppercase
        assert!(looks_like_country_code("KEN"));
        assert!(looks_like_country_code("USA"));
        assert!(looks_like_country_code("GBR"));
        // Mixed case — not a code
        assert!(!looks_like_country_code("Kenya"));
        assert!(!looks_like_country_code("Us"));
        // Lowercase — not a code
        assert!(!looks_like_country_code("us"));
        // Too long
        assert!(!looks_like_country_code("ABCD"));
        // Too short
        assert!(!looks_like_country_code("A"));
        // Multi-word
        assert!(!looks_like_country_code("united states"));
        // Whitespace padded — still valid after trim
        assert!(looks_like_country_code(" US "));
    }

    #[test]
    fn format_population_with_commas() {
        assert_eq!(format_population(0), "0");
        assert_eq!(format_population(999), "999");
        assert_eq!(format_population(1000), "1,000");
        assert_eq!(format_population(53_771_296), "53,771,296");
        assert_eq!(format_population(1_000_000_000), "1,000,000,000");
        assert_eq!(format_population(12_345), "12,345");
    }

    #[test]
    fn format_country_handles_full_entry() {
        let entry = serde_json::json!({
            "name": {
                "common": "Kenya",
                "official": "Republic of Kenya"
            },
            "capital": ["Nairobi"],
            "population": 53771296,
            "currencies": {
                "KES": { "name": "Kenyan shilling", "symbol": "KSh" }
            },
            "languages": {
                "eng": "English",
                "swa": "Swahili"
            },
            "region": "Africa",
            "subregion": "Eastern Africa",
            "flags": { "emoji": "\u{1f1f0}\u{1f1ea}" }
        });
        let result = format_country(&entry);
        assert!(result.contains("**Kenya** (Republic of Kenya)"));
        assert!(result.contains("Africa (Eastern Africa)"));
        assert!(result.contains("Nairobi"));
        assert!(result.contains("53,771,296"));
        assert!(result.contains("Kenyan shilling (KES)"));
        assert!(result.contains("English"));
        assert!(result.contains("Swahili"));
    }

    #[test]
    fn format_country_handles_minimal_entry() {
        let entry = serde_json::json!({
            "name": { "common": "Testland" },
            "region": "Testregion",
            "population": 0
        });
        let result = format_country(&entry);
        assert!(result.contains("**Testland**"));
        assert!(result.contains("Testregion"));
        assert!(result.contains("N/A")); // missing capital, currencies, languages
    }

    #[test]
    fn format_product_handles_full_entry() {
        let product = serde_json::json!({
            "product_name": "Nutella",
            "brands": "Ferrero",
            "nutrition_grades": "e",
            "nutriments": { "energy-kcal_100g": 539.0 },
            "ingredients_text": "Sugar, palm oil, hazelnuts (13%), fat-reduced cocoa",
            "allergens": "en:milk, en:nuts, en:soybeans"
        });
        let result = format_product(&product);
        assert!(result.contains("**Nutella** by Ferrero"));
        assert!(result.contains("Nutri-Score: E"));
        assert!(result.contains("539 kcal/100g"));
        assert!(result.contains("Sugar, palm oil"));
        assert!(result.contains("milk"));
    }

    #[test]
    fn format_product_handles_minimal_entry() {
        let product = serde_json::json!({
            "product_name": "Mystery Item"
        });
        let result = format_product(&product);
        assert!(result.contains("**Mystery Item**"));
        assert!(result.contains("Nutri-Score: N/A"));
    }

    #[test]
    fn extract_domain_works() {
        assert_eq!(
            extract_domain("https://www.example.com/path"),
            "example.com"
        );
        assert_eq!(extract_domain("https://example.com/foo"), "example.com");
        assert_eq!(
            extract_domain("http://blog.rust-lang.org/2025/post"),
            "blog.rust-lang.org"
        );
        assert_eq!(extract_domain(""), "");
    }

    // ── resolve_* tests (no ToolCaller configured) ───────────────────────

    #[tokio::test]
    async fn resolve_country_from_param() {
        let params = CountryInfoParams {
            country: Some("Kenya".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_country(&params).await, "Kenya");
    }

    #[tokio::test]
    async fn resolve_country_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "name".to_string(),
            serde_json::Value::String("Japan".to_string()),
        );
        let params = CountryInfoParams {
            country: None,
            extra,
        };
        assert_eq!(resolve_country(&params).await, "Japan");
    }

    #[tokio::test]
    async fn resolve_country_empty_when_nothing_provided() {
        let params = CountryInfoParams {
            country: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_country(&params).await, "");
    }

    #[tokio::test]
    async fn resolve_product_barcode_from_param() {
        let params = ProductLookupParams {
            barcode: Some("3017620422003".to_string()),
            name: None,
            limit: None,
            extra: Default::default(),
        };
        let (barcode, name) = resolve_product(&params).await;
        assert_eq!(barcode, Some("3017620422003".to_string()));
        assert_eq!(name, None);
    }

    #[tokio::test]
    async fn resolve_product_name_from_param() {
        let params = ProductLookupParams {
            barcode: None,
            name: Some("nutella".to_string()),
            limit: None,
            extra: Default::default(),
        };
        let (barcode, name) = resolve_product(&params).await;
        assert_eq!(barcode, None);
        assert_eq!(name, Some("nutella".to_string()));
    }

    #[tokio::test]
    async fn resolve_product_empty_when_nothing_provided() {
        let params = ProductLookupParams {
            barcode: None,
            name: None,
            limit: None,
            extra: Default::default(),
        };
        let (barcode, name) = resolve_product(&params).await;
        assert_eq!(barcode, None);
        assert_eq!(name, None);
    }

    #[tokio::test]
    async fn resolve_web_query_from_param() {
        let params = WebSearchParams {
            query: Some("Rust programming".to_string()),
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_web_query(&params).await, "Rust programming");
    }

    #[tokio::test]
    async fn resolve_web_query_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "q".to_string(),
            serde_json::Value::String("best coffee".to_string()),
        );
        let params = WebSearchParams {
            query: None,
            limit: None,
            extra,
        };
        assert_eq!(resolve_web_query(&params).await, "best coffee");
    }

    #[tokio::test]
    async fn resolve_web_query_empty_when_nothing_provided() {
        let params = WebSearchParams {
            query: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_web_query(&params).await, "");
    }

    #[tokio::test]
    async fn resolve_price_product_from_param() {
        let params = ProductPriceParams {
            product: Some("3017620422003".to_string()),
            location: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_price_product(&params).await, "3017620422003");
    }

    // ── Live integration tests ───────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_rest_countries_kenya() {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/name/Kenya?fields={}",
            REST_COUNTRIES_BASE, COUNTRY_FIELDS,
        );
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("REST Countries request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.expect("failed to parse response");
        let arr = body.as_array().expect("should be an array");
        assert!(!arr.is_empty());
        let capitals = arr[0]["capital"].as_array().expect("should have capital");
        let cap = capitals[0].as_str().expect("capital should be string");
        assert_eq!(cap, "Nairobi");
        eprintln!("Kenya capital: {}", cap);
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_open_food_facts_barcode() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/test")
            .build()
            .unwrap();
        let url = format!("{}/api/v2/product/3017620422003", OFF_BASE);
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .expect("OFF request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.expect("failed to parse response");
        let status = body["status"].as_u64().unwrap_or(0);
        assert_eq!(status, 1, "product should be found");
        let name = body["product"]["product_name"].as_str().unwrap_or("");
        eprintln!("Product: {}", name);
        assert!(
            name.to_lowercase().contains("nutella"),
            "barcode 3017620422003 should be Nutella"
        );
    }

    // ── get_product_price edge case tests ──────────────────────────────

    #[test]
    fn format_price_item_handles_missing_location() {
        // Simulate a price item where location is null
        let item = serde_json::json!({
            "price": 3.52,
            "currency": "EUR",
            "date": "2026-04-30",
            "location": null,
            "product": { "product_name": "Nutella" }
        });
        let price = item["price"].as_f64().unwrap();
        let currency = item["currency"].as_str().unwrap_or("???");
        let location = item["location"]["osm_name"]
            .as_str()
            .unwrap_or("Unknown location");
        let date = item["date"].as_str().unwrap_or("unknown date");
        let formatted = format!("{:.2} {} at {} ({})", price, currency, location, date);
        assert_eq!(formatted, "3.52 EUR at Unknown location (2026-04-30)");
    }

    #[test]
    fn format_price_item_handles_missing_osm_name() {
        // location exists but osm_name is absent
        let item = serde_json::json!({
            "price": 2.99,
            "currency": "USD",
            "date": "2026-05-01",
            "location": { "osm_id": 12345 },
            "product": { "product_name": "Test" }
        });
        let location = item["location"]["osm_name"]
            .as_str()
            .unwrap_or("Unknown location");
        assert_eq!(location, "Unknown location");
    }

    #[test]
    fn format_price_item_handles_missing_product() {
        // product is null — the product_name extraction should fall back
        let item = serde_json::json!({
            "price": 1.50,
            "currency": "GBP",
            "date": "2026-03-15",
            "location": { "osm_name": "Tesco" },
            "product": null
        });
        let product_name = item["product"]["product_name"]
            .as_str()
            .unwrap_or("Product");
        assert_eq!(product_name, "Product");
    }

    #[test]
    fn format_price_item_handles_missing_currency() {
        let item = serde_json::json!({
            "price": 4.00,
            "date": "2026-05-10",
            "location": { "osm_name": "Aldi" }
        });
        let currency = item["currency"].as_str().unwrap_or("???");
        assert_eq!(currency, "???");
    }

    #[test]
    fn format_price_items_skips_missing_price() {
        // The filter_map in get_product_price uses as_f64()? — items without
        // a numeric price field should be silently skipped.
        let items_json = serde_json::json!([
            { "price": 3.52, "currency": "EUR", "date": "2026-04-30",
              "location": { "osm_name": "Colruyt" } },
            { "currency": "USD", "date": "2026-05-01",
              "location": { "osm_name": "Walmart" } },
            { "price": null, "currency": "GBP", "date": "2026-05-02",
              "location": { "osm_name": "Tesco" } }
        ]);
        let arr = items_json.as_array().unwrap();
        let formatted: Vec<String> = arr
            .iter()
            .filter_map(|item| {
                let price = item["price"].as_f64()?;
                let currency = item["currency"].as_str().unwrap_or("???");
                let location = item["location"]["osm_name"]
                    .as_str()
                    .unwrap_or("Unknown location");
                let date = item["date"].as_str().unwrap_or("unknown date");
                Some(format!(
                    "{:.2} {} at {} ({})",
                    price, currency, location, date
                ))
            })
            .collect();
        // Only the first item has a valid price
        assert_eq!(formatted.len(), 1);
        assert_eq!(formatted[0], "3.52 EUR at Colruyt (2026-04-30)");
    }

    #[test]
    fn format_price_empty_items_array() {
        let items_json = serde_json::json!([]);
        let arr = items_json.as_array().unwrap();
        let formatted: Vec<String> = arr
            .iter()
            .filter_map(|item| {
                let price = item["price"].as_f64()?;
                let currency = item["currency"].as_str().unwrap_or("???");
                Some(format!("{:.2} {}", price, currency))
            })
            .collect();
        assert!(formatted.is_empty());
    }

    // ── get_country_info edge case tests ────────────────────────────────

    #[test]
    fn country_code_lowercase_not_detected_as_code() {
        // Lowercase "us" is NOT detected as a country code by looks_like_country_code,
        // so it routes through /name/ endpoint instead of /alpha/.
        // REST Countries /name/ accepts lowercase and returns results.
        assert!(!looks_like_country_code("us"));
        assert!(!looks_like_country_code("ke"));
        assert!(!looks_like_country_code("gbr"));
    }

    #[tokio::test]
    async fn resolve_country_trims_whitespace() {
        let params = CountryInfoParams {
            country: Some("  Kenya  ".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_country(&params).await, "Kenya");
    }

    // ── lookup_product edge case tests ──────────────────────────────────

    #[test]
    fn format_product_handles_very_long_ingredients() {
        // Ingredients longer than 200 chars should be truncated with "..."
        let long_ingredients = "a".repeat(300);
        let product = serde_json::json!({
            "product_name": "Long Product",
            "ingredients_text": long_ingredients
        });
        let result = format_product(&product);
        assert!(result.contains("..."));
        // The ingredients portion should be capped
        let ingredients_line = result
            .lines()
            .find(|l| l.starts_with("Ingredients:"))
            .expect("should have ingredients line");
        // 200 chars of 'a' + "..." = 203, plus "Ingredients: " prefix
        assert!(ingredients_line.len() < 250);
    }

    // ── Live integration tests ──────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_open_prices_nutella() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/0.1 (GIAP MCP)")
            .build()
            .unwrap();
        let url = format!(
            "{}/prices?product_code=3017620422003&order_by=-date&page_size=5",
            OFF_PRICES_BASE,
        );
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("Open Prices request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.expect("parse failed");
        let total = body["total"].as_u64().unwrap_or(0);
        eprintln!("Nutella prices: {} total entries", total);
        assert!(total > 0, "should have prices for Nutella");
        let items = body["items"].as_array().expect("items array missing");
        assert!(!items.is_empty(), "should return at least one price");
        let first = &items[0];
        let price = first["price"].as_f64().expect("price field missing");
        eprintln!(
            "First price: {} {}",
            price,
            first["currency"].as_str().unwrap_or("?")
        );
        assert!(price > 0.0, "price should be positive");
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_rest_countries_nonexistent() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/0.1 (GIAP MCP)")
            .build()
            .unwrap();
        let url = format!("{}/name/Wakanda", REST_COUNTRIES_BASE);
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("request failed");
        // REST Countries returns 404 for non-existent countries
        assert_eq!(resp.status().as_u16(), 404);
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_open_food_facts_nonexistent_barcode() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/0.1 (GIAP MCP)")
            .build()
            .unwrap();
        let url = format!("{}/api/v2/product/0000000000000", OFF_BASE);
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("request failed");
        assert!(resp.status().is_success()); // OFF returns 200 with status=0 for not found
        let body: serde_json::Value = resp.json().await.expect("parse failed");
        let status = body["status"].as_u64().unwrap_or(0);
        assert_eq!(status, 0, "should return status 0 for non-existent barcode");
    }
}
