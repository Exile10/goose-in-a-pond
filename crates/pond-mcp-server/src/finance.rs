//! Finance MCP Server — currency exchange, stock quotes, and crypto prices.
//!
//! Provides 4 tools: `get_exchange_rate` (Frankfurter), `convert_currency` (Frankfurter),
//! `get_stock_quote` (Finnhub), `get_crypto_price` (CoinGecko).
//! Depends on a `reqwest::Client` for HTTP fetches. The Finnhub API key comes
//! from the pond's `SecretRepository` via `crate::secrets` — never from
//! `Settings`, which `GET /api/v1/settings` serialises wholesale (PAI-2 P2).

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
pub struct ExchangeRateParams {
    /// Base currency code, default USD.
    pub from: Option<String>,
    /// Comma-separated target codes; omit for majors.
    pub to: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ConvertCurrencyParams {
    pub amount: Option<f64>,
    /// Source currency code, default USD.
    pub from: Option<String>,
    /// Target currency code; required.
    pub to: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct StockQuoteParams {
    /// Ticker symbol, e.g. AAPL.
    pub symbol: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CryptoPriceParams {
    /// Name or symbol, e.g. bitcoin or BTC.
    pub asset: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Constants ──────────────────────────────────────────────────────────────

const FRANKFURTER_BASE_URL: &str = "https://api.frankfurter.dev/v1";
const FINNHUB_BASE_URL: &str = "https://finnhub.io/api/v1";

/// Free tier. Without it `get_stock_quote` falls back to an UNOFFICIAL Yahoo
/// endpoint, which is a real answer from a source nobody supports.
const FINNHUB_SIGNUP: &str = "https://finnhub.io/register";
const COINGECKO_BASE_URL: &str = "https://api.coingecko.com/api/v3";
const YAHOO_FINANCE_BASE_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

const EXCHANGE_RATE_BUDGET: usize = 500;
const CONVERT_CURRENCY_BUDGET: usize = 300;
const STOCK_QUOTE_BUDGET: usize = 300;
const CRYPTO_PRICE_BUDGET: usize = 400;

/// Default target currencies when none specified (major currencies + KES).
const DEFAULT_TARGET_CURRENCIES: &str = "EUR,GBP,JPY,KES,CHF,CAD,AUD";

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct FinanceMcpServer {
    http_client: reqwest::Client,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl FinanceMcpServer {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Get current forex rates. To convert a specific amount, use convert_currency instead."
    )]
    async fn get_exchange_rate(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ExchangeRateParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_exchange_rate");
        let from = resolve_currency_code(
            params.0.from.as_deref(),
            &params.0.extra,
            &["base", "currency", "source"],
        )
        .unwrap_or_else(|| "USD".to_string());
        let to = resolve_currency_code(
            params.0.to.as_deref(),
            &params.0.extra,
            &["target", "to", "symbols"],
        )
        .unwrap_or_else(|| DEFAULT_TARGET_CURRENCIES.to_string());

        eprintln!("[finance] get_exchange_rate: from={}, to={}", from, to);

        let url = format!(
            "{}/latest?base={}&symbols={}",
            FRANKFURTER_BASE_URL,
            urlencoding::encode(&from),
            urlencoding::encode(&to),
        );
        eprintln!("[finance] GET {}", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[finance] Frankfurter request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Frankfurter (exchange rates)", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[finance] Frankfurter returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error(
                    "Frankfurter (exchange rates)",
                    &format!("HTTP {status}"),
                ),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[finance] failed to parse Frankfurter response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Frankfurter", &e.to_string()),
                )]));
            }
        };

        let base = body["base"].as_str().unwrap_or(&from);
        let date = body["date"].as_str().unwrap_or("unknown");
        let rates = body["rates"].as_object();

        let rate_parts: Vec<String> = rates
            .map(|m| {
                m.iter()
                    .map(|(currency, rate)| {
                        format!("{:.4} {}", rate.as_f64().unwrap_or(0.0), currency)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let text = if rate_parts.is_empty() {
            crate::format::format_no_results(
                &format!("exchange rate data for {} (as of {})", base, date),
                &["giap-knowledge__compute_answer"],
            )
        } else {
            format!(
                "1 {} = {} (ECB data, {})",
                base,
                rate_parts.join(" | "),
                date,
            )
        };

        let truncated = crate::format::truncate_to_budget(&text, EXCHANGE_RATE_BUDGET);
        eprintln!(
            "[finance] get_exchange_rate done, {} chars",
            truncated.len()
        );

        // Build UI hint from first rate pair (primary conversion)
        if let Some(rates_obj) = rates {
            if let Some((first_currency, first_rate)) = rates_obj.iter().next() {
                let ui_data = serde_json::json!({
                    "from": base,
                    "to": first_currency,
                    "rate": first_rate.as_f64().unwrap_or(0.0),
                });
                let hint = format!("[[[mcp-ui:exchange:{}]]]\n", ui_data);
                let full_result = format!("{}{}", hint, truncated);
                return Ok(CallToolResult::success(vec![Content::text(full_result)]));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(truncated)]))
    }

    #[tool(
        description = "Convert an amount between currencies. amount default 1, from default USD; to required."
    )]
    async fn convert_currency(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ConvertCurrencyParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("convert_currency");
        let amount = resolve_amount(params.0.amount, &params.0.extra);
        let from = resolve_currency_code(
            params.0.from.as_deref(),
            &params.0.extra,
            &["from", "source", "base"],
        )
        .unwrap_or_else(|| "USD".to_string());
        let to = resolve_currency_code(params.0.to.as_deref(), &params.0.extra, &["to", "target"]);

        eprintln!(
            "[finance] convert_currency: amount={}, from={}, to={:?}",
            amount, from, to
        );

        let to = match to {
            Some(t) => t,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "I need a target currency. Say something like 'convert 100 USD to EUR'.",
                )]));
            }
        };

        let url = format!(
            "{}/latest?amount={}&from={}&to={}",
            FRANKFURTER_BASE_URL,
            amount,
            urlencoding::encode(&from),
            urlencoding::encode(&to),
        );
        eprintln!("[finance] GET {}", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[finance] Frankfurter convert failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Frankfurter (conversion)", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[finance] Frankfurter convert returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error(
                    "Frankfurter (conversion)",
                    &format!("HTTP {status}"),
                ),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[finance] failed to parse Frankfurter convert response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Frankfurter", &e.to_string()),
                )]));
            }
        };

        let date = body["date"].as_str().unwrap_or("unknown");
        let converted = body["rates"][&to].as_f64();

        let text = match converted {
            Some(result) => {
                let rate = if amount > 0.0 { result / amount } else { 0.0 };
                format!(
                    "{:.2} {} = {:.2} {} (rate: {:.4}, as of {})",
                    amount, from, result, to, rate, date,
                )
            }
            None => format!(
                "Could not convert {} {} to {}. Check the currency codes.",
                amount, from, to,
            ),
        };

        let truncated = crate::format::truncate_to_budget(&text, CONVERT_CURRENCY_BUDGET);
        eprintln!("[finance] convert_currency done, {} chars", truncated.len());
        Ok(CallToolResult::success(vec![Content::text(truncated)]))
    }

    #[tool(description = "Get a stock's current price and daily change by ticker symbol.")]
    async fn get_stock_quote(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<StockQuoteParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_stock_quote");
        // 1. Resolve symbol first (needed for both paths)
        let symbol = resolve_stock_symbol(params.0.symbol.as_deref(), &params.0.extra);
        eprintln!("[finance] get_stock_quote: symbol={:?}", symbol);

        let symbol = match symbol {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "I need a stock ticker symbol (e.g. AAPL, MSFT, TSLA). \
                     Retry with a 'symbol' parameter.",
                )]));
            }
        };

        // 2. Finnhub API key — from the secret store (PAI-2 P2). A missing
        //    key is not an error; the tool degrades to the keyless Yahoo
        //    fallback, which is also what happens when no entry point
        //    installed a secret store.
        let finnhub_key = crate::secrets::secret("FINNHUB_API_KEY")
            .await
            .filter(|k| !k.trim().is_empty());

        if let Some(api_key) = finnhub_key {
            return self.get_stock_quote_finnhub(api_key.trim(), &symbol).await;
        }

        // Fallback: Yahoo Finance v8 unofficial (no API key required).
        //
        // A real quote from an endpoint nobody supports. `format_degraded` puts
        // that in the result where a user can learn it; the `eprintln!` this
        // replaces put it on the server's stderr, and `tracing` puts it in the
        // log the audit trail actually reads.
        tracing::warn!(
            target: "giap::trace",
            kind = "tool_degraded",
            tool = "get_stock_quote",
            missing = "FINNHUB_API_KEY",
            "no Finnhub key — quoting from the unofficial Yahoo endpoint"
        );
        let result = self.get_stock_quote_yahoo(&symbol).await?;
        Ok(crate::format::degrade_result(
            result,
            "Finnhub API key",
            FINNHUB_SIGNUP,
        ))
    }

    #[tool(description = "Get cryptocurrency price and 24h market data by name or symbol.")]
    async fn get_crypto_price(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<CryptoPriceParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_crypto_price");
        let asset = resolve_crypto_asset(params.0.asset.as_deref(), &params.0.extra);

        eprintln!("[finance] get_crypto_price: asset={:?}", asset);

        let asset = match asset {
            Some(a) => a,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "I need a cryptocurrency name or symbol (e.g. Bitcoin, BTC, Ethereum, ETH). \
                     Retry with an 'asset' parameter.",
                )]));
            }
        };

        // CoinGecko simple/price endpoint — returns price, 24h change, market cap, volume
        let url = format!(
            "{}/simple/price?ids={}&vs_currencies=usd&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true",
            COINGECKO_BASE_URL,
            urlencoding::encode(&asset),
        );
        eprintln!("[finance] GET {}", url);

        let body = match self.fetch_json(&url).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("CoinGecko", &e),
                )]));
            }
        };

        // CoinGecko returns: { "bitcoin": { "usd": 67234, "usd_24h_change": 2.34, ... } }
        let data = match body.get(&asset) {
            Some(d) => d,
            None => {
                // Asset ID not recognized — try search endpoint
                let search_url = format!(
                    "{}/search?query={}",
                    COINGECKO_BASE_URL,
                    urlencoding::encode(&asset),
                );
                eprintln!("[finance] ID not found, searching: GET {}", search_url);

                match self.fetch_json(&search_url).await {
                    Ok(search_body) => {
                        if let Some(coins) = search_body["coins"].as_array() {
                            if let Some(first) = coins.first() {
                                let coin_id = first["id"].as_str().unwrap_or("");
                                let coin_name = first["name"].as_str().unwrap_or("Unknown");
                                let coin_symbol = first["symbol"].as_str().unwrap_or("???");

                                if coin_id.is_empty() {
                                    return Ok(CallToolResult::success(vec![Content::text(
                                        crate::format::format_no_results(
                                            &format!("a cryptocurrency matching '{}'", asset),
                                            &["giap-knowledge__compute_answer"],
                                        ),
                                    )]));
                                }

                                // Fetch price for the found coin
                                let price_url = format!(
                                    "{}/simple/price?ids={}&vs_currencies=usd&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true",
                                    COINGECKO_BASE_URL, coin_id,
                                );
                                match self.fetch_json(&price_url).await {
                                    Ok(price_body) => {
                                        if let Some(d) = price_body.get(coin_id) {
                                            let text =
                                                format_coingecko_price(coin_name, coin_symbol, d);
                                            let truncated = crate::format::truncate_to_budget(
                                                &text,
                                                CRYPTO_PRICE_BUDGET,
                                            );
                                            eprintln!(
                                                "[finance] get_crypto_price done, {} chars",
                                                truncated.len()
                                            );
                                            let ui_data =
                                                build_crypto_ui_data(coin_name, coin_symbol, d);
                                            let hint = format!("[[[mcp-ui:crypto:{}]]]\n", ui_data);
                                            let full_result = format!("{}{}", hint, truncated);
                                            return Ok(CallToolResult::success(vec![
                                                Content::text(full_result),
                                            ]));
                                        }
                                    }
                                    Err(e) => {
                                        return Ok(CallToolResult::success(vec![Content::text(
                                            crate::format::format_api_error("CoinGecko", &e),
                                        )]));
                                    }
                                }

                                return Ok(CallToolResult::success(vec![Content::text(format!(
                                    "Found '{}' ({}) but could not fetch its price.",
                                    coin_name, coin_symbol,
                                ))]));
                            }
                        }

                        return Ok(CallToolResult::success(vec![Content::text(
                            crate::format::format_no_results(
                                &format!("a cryptocurrency matching '{}'", asset),
                                &["giap-knowledge__compute_answer"],
                            ),
                        )]));
                    }
                    Err(e) => {
                        return Ok(CallToolResult::success(vec![Content::text(
                            crate::format::format_api_error("CoinGecko", &e),
                        )]));
                    }
                }
            }
        };

        // Format the direct-hit result — we need the display name
        // CoinGecko simple/price doesn't return name/symbol, so capitalize the ID
        let display_name = asset
            .split('-')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().to_string() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let symbol = asset.to_uppercase().chars().take(5).collect::<String>();

        let text = format_coingecko_price(&display_name, &symbol, data);
        let truncated = crate::format::truncate_to_budget(&text, CRYPTO_PRICE_BUDGET);
        eprintln!("[finance] get_crypto_price done, {} chars", truncated.len());

        let ui_data = build_crypto_ui_data(&display_name, &symbol, data);
        let hint = format!("[[[mcp-ui:crypto:{}]]]\n", ui_data);
        let full_result = format!("{}{}", hint, truncated);
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }
}

#[tool_handler]
impl ServerHandler for FinanceMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-finance",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Finance server — currency, stocks, and cryptocurrency.\n\n\
                 Tools: get_exchange_rate (forex rates), convert_currency (amount conversion), \
                 get_stock_quote (stock prices), get_crypto_price (crypto prices).\n\n\
                 All tools work without API keys. Finnhub key adds detailed stock data.\n\
                 For 'how much is X in Y': use convert_currency. For rate comparison: get_exchange_rate.\n\
                 For stock prices: get_stock_quote with ticker symbol. For crypto: get_crypto_price.\n\
                 Always note: this is informational, not financial advice.",
            )
    }
}

// ── Extracted tool implementations ────────────────────────────────────────

impl FinanceMcpServer {
    /// Finnhub API path for get_stock_quote (preferred when key is configured).
    async fn get_stock_quote_finnhub(
        &self,
        api_key: &str,
        symbol: &str,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let url = format!(
            "{}/quote?symbol={}&token={}",
            FINNHUB_BASE_URL,
            urlencoding::encode(symbol),
            urlencoding::encode(api_key),
        );
        eprintln!("[finance] GET {}", url.replace(api_key, "***"));

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[finance] Finnhub request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Finnhub", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[finance] Finnhub returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Finnhub", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[finance] failed to parse Finnhub response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Finnhub", &e.to_string()),
                )]));
            }
        };

        // Finnhub returns all zeros for invalid symbols
        let current = body["c"].as_f64().unwrap_or(0.0);
        let change = body["d"].as_f64().unwrap_or(0.0);
        let change_pct = body["dp"].as_f64().unwrap_or(0.0);
        let high = body["h"].as_f64().unwrap_or(0.0);
        let low = body["l"].as_f64().unwrap_or(0.0);
        let open = body["o"].as_f64().unwrap_or(0.0);
        let prev_close = body["pc"].as_f64().unwrap_or(0.0);

        if current == 0.0 && open == 0.0 && high == 0.0 && low == 0.0 {
            eprintln!("[finance] Finnhub returned all zeros for '{}'", symbol);
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!(
                        "a stock quote for '{}' (standard US exchange symbols only, \
                         e.g. AAPL, MSFT, GOOGL)",
                        symbol
                    ),
                    &["giap-knowledge__compute_answer"],
                ),
            )]));
        }

        let sign = if change >= 0.0 { "+" } else { "" };
        let text = format!(
            "{}: ${:.2} ({}{:.2}%) | Open: ${:.2} | High: ${:.2} | Low: ${:.2} | Prev Close: ${:.2}",
            symbol, current, sign, change_pct, open, high, low, prev_close,
        );

        let truncated = crate::format::truncate_to_budget(&text, STOCK_QUOTE_BUDGET);
        eprintln!(
            "[finance] get_stock_quote (Finnhub) done, {} chars",
            truncated.len()
        );

        let ui_data = serde_json::json!({
            "symbol": symbol,
            "name": symbol,
            "price": current,
            "change": change,
            "change_pct": change_pct,
        });
        let hint = format!("[[[mcp-ui:stock:{}]]]\n", ui_data);
        let full_result = format!("{}{}", hint, truncated);
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }

    /// Yahoo Finance v8 unofficial fallback for get_stock_quote (no API key needed).
    async fn get_stock_quote_yahoo(
        &self,
        symbol: &str,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let url = format!(
            "{}/{}?interval=1d&range=1d",
            YAHOO_FINANCE_BASE_URL,
            urlencoding::encode(symbol),
        );
        eprintln!("[finance] GET {} (Yahoo Finance unofficial)", url);

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.header("user-agent", "Mozilla/5.0 (compatible; GIAP/0.1)")
                .timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[finance] Yahoo Finance request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Yahoo Finance", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[finance] Yahoo Finance returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Yahoo Finance", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[finance] failed to parse Yahoo Finance response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Yahoo Finance", &e.to_string()),
                )]));
            }
        };

        // Parse: chart.result[0].meta
        let meta = &body["chart"]["result"][0]["meta"];
        let price = meta["regularMarketPrice"].as_f64().unwrap_or(0.0);
        let prev_close = meta["chartPreviousClose"].as_f64().unwrap_or(0.0);
        let currency = meta["currency"].as_str().unwrap_or("USD");
        let exchange = meta["exchangeName"].as_str().unwrap_or("");

        if price == 0.0 && prev_close == 0.0 {
            eprintln!("[finance] Yahoo Finance returned no data for '{}'", symbol);
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!(
                        "a stock quote for '{}' (standard US exchange symbols only, \
                         e.g. AAPL, MSFT, GOOGL)",
                        symbol
                    ),
                    &["giap-knowledge__compute_answer"],
                ),
            )]));
        }

        let change = price - prev_close;
        let change_pct = if prev_close > 0.0 {
            (change / prev_close) * 100.0
        } else {
            0.0
        };

        let sign = if change >= 0.0 { "+" } else { "" };
        let currency_sym = match currency {
            "USD" => "$",
            "EUR" => "E",
            "GBP" => "L",
            _ => "",
        };

        let exchange_note = if exchange.is_empty() {
            String::new()
        } else {
            format!(" ({})", exchange)
        };

        let text = format!(
            "{}: {}{:.2} ({}{:.2}%) | Open: \u{2014} | High: \u{2014} | Low: \u{2014} | Prev Close: {}{:.2}{}\n\
             (Source: Yahoo Finance \u{2014} unofficial, may be delayed)",
            symbol,
            currency_sym,
            price,
            sign,
            change_pct,
            currency_sym,
            prev_close,
            exchange_note,
        );

        let truncated = crate::format::truncate_to_budget(&text, STOCK_QUOTE_BUDGET);
        eprintln!(
            "[finance] get_stock_quote (Yahoo) done, {} chars",
            truncated.len()
        );

        let ui_data = serde_json::json!({
            "symbol": symbol,
            "name": symbol,
            "price": price,
            "change": change,
            "change_pct": change_pct,
        });
        let hint = format!("[[[mcp-ui:stock:{}]]]\n", ui_data);
        let full_result = format!("{}{}", hint, truncated);
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

impl FinanceMcpServer {
    /// Fetch a URL and return the parsed JSON body.
    async fn fetch_json(&self, url: &str) -> Result<serde_json::Value, String> {
        let resp = crate::http::traced_get_with(&self.http_client, url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| e.to_string())
    }
}

/// Build structured UI data for a crypto price card.
fn build_crypto_ui_data(name: &str, symbol: &str, data: &serde_json::Value) -> serde_json::Value {
    let price = data["usd"].as_f64().unwrap_or(0.0);
    let change = data["usd_24h_change"].as_f64().unwrap_or(0.0);
    serde_json::json!({
        "coins": [{
            "symbol": symbol,
            "name": name,
            "price": format!("${}", format_price(price)),
            "change": change,
        }]
    })
}

/// Format a CoinGecko price response into a display string.
fn format_coingecko_price(name: &str, symbol: &str, data: &serde_json::Value) -> String {
    let price = data["usd"].as_f64().unwrap_or(0.0);
    let change = data["usd_24h_change"].as_f64().unwrap_or(0.0);
    let mcap = data["usd_market_cap"].as_f64().unwrap_or(0.0);
    let volume = data["usd_24h_vol"].as_f64().unwrap_or(0.0);

    let sign = if change >= 0.0 { "+" } else { "" };
    format!(
        "{} ({}): ${} ({}{:.2}% 24h) | Market Cap: ${} | Volume: ${}",
        name,
        symbol,
        format_price(price),
        sign,
        change,
        format_large_number(mcap),
        format_large_number(volume),
    )
}

/// Normalize a currency code to uppercase 3-letter format.
fn normalize_currency_code(code: &str) -> String {
    code.trim().to_uppercase()
}

/// Resolve a currency code from the primary param, falling back to extras.
fn resolve_currency_code(
    primary: Option<&str>,
    extras: &std::collections::HashMap<String, serde_json::Value>,
    extra_keys: &[&str],
) -> Option<String> {
    // 1. Primary param
    if let Some(code) = primary {
        let normalized = normalize_currency_code(code);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    // 2. Scan extras
    for key in extra_keys {
        if let Some(val) = extras.get(*key) {
            if let Some(s) = val.as_str() {
                let normalized = normalize_currency_code(s);
                if !normalized.is_empty() {
                    return Some(normalized);
                }
            }
        }
    }

    None
}

/// Resolve a numeric amount from primary param or extras.
fn resolve_amount(
    primary: Option<f64>,
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> f64 {
    if let Some(amt) = primary {
        if amt > 0.0 {
            return amt;
        }
    }

    for key in &["amount", "value", "sum"] {
        if let Some(val) = extras.get(*key) {
            if let Some(n) = val.as_f64() {
                if n > 0.0 {
                    return n;
                }
            }
            // Also try parsing string values
            if let Some(s) = val.as_str() {
                if let Ok(n) = s.parse::<f64>() {
                    if n > 0.0 {
                        return n;
                    }
                }
            }
        }
    }

    1.0
}

/// Resolve a stock symbol from params/extras.
/// Strips leading '$' and uppercases.
fn resolve_stock_symbol(
    primary: Option<&str>,
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<String> {
    // 1. Primary param
    if let Some(sym) = primary {
        let cleaned = normalize_stock_symbol(sym);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }

    // 2. Scan extras
    for key in &["stock", "ticker", "company", "symbol"] {
        if let Some(val) = extras.get(*key) {
            if let Some(s) = val.as_str() {
                let cleaned = normalize_stock_symbol(s);
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
    }

    None
}

/// Normalize stock symbol: strip '$' prefix, uppercase.
fn normalize_stock_symbol(raw: &str) -> String {
    raw.trim().trim_start_matches('$').trim().to_uppercase()
}

/// Map common crypto symbols/names to CoinGecko asset IDs.
fn crypto_symbol_to_coingecko_id(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    match lower.as_str() {
        "btc" | "bitcoin" => "bitcoin".to_string(),
        "eth" | "ethereum" | "ether" => "ethereum".to_string(),
        "sol" | "solana" => "solana".to_string(),
        "ada" | "cardano" => "cardano".to_string(),
        "dot" | "polkadot" => "polkadot".to_string(),
        "doge" | "dogecoin" => "dogecoin".to_string(),
        "xrp" | "ripple" => "ripple".to_string(),
        "avax" | "avalanche" => "avalanche-2".to_string(),
        "matic" | "polygon" => "matic-network".to_string(),
        "link" | "chainlink" => "chainlink".to_string(),
        "ltc" | "litecoin" => "litecoin".to_string(),
        "uni" | "uniswap" => "uniswap".to_string(),
        "bnb" | "binance coin" | "binance" => "binancecoin".to_string(),
        "usdt" | "tether" => "tether".to_string(),
        "usdc" | "usd coin" => "usd-coin".to_string(),
        "shib" | "shiba inu" | "shiba" => "shiba-inu".to_string(),
        "atom" | "cosmos" => "cosmos".to_string(),
        "xlm" | "stellar" => "stellar".to_string(),
        "trx" | "tron" => "tron".to_string(),
        _ => lower.replace(' ', "-"),
    }
}

/// Resolve a crypto asset identifier from params/extras.
fn resolve_crypto_asset(
    primary: Option<&str>,
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<String> {
    // 1. Primary param
    if let Some(asset) = primary {
        let trimmed = asset.trim();
        if !trimmed.is_empty() {
            return Some(crypto_symbol_to_coingecko_id(trimmed));
        }
    }

    // 2. Scan extras
    for key in &["coin", "crypto", "token", "currency", "name", "symbol"] {
        if let Some(val) = extras.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(crypto_symbol_to_coingecko_id(trimmed));
                }
            }
        }
    }

    None
}

/// Format a large number with K/M/B/T suffixes.
/// 1_500_000_000.0 -> "1.50B", 1_500_000.0 -> "1.50M", 1_500.0 -> "1.50K"
pub fn format_large_number(n: f64) -> String {
    let abs = n.abs();
    let sign = if n < 0.0 { "-" } else { "" };

    if abs >= 1_000_000_000_000.0 {
        format!("{}{:.2}T", sign, abs / 1_000_000_000_000.0)
    } else if abs >= 1_000_000_000.0 {
        format!("{}{:.2}B", sign, abs / 1_000_000_000.0)
    } else if abs >= 1_000_000.0 {
        format!("{}{:.2}M", sign, abs / 1_000_000.0)
    } else if abs >= 1_000.0 {
        format!("{}{:.2}K", sign, abs / 1_000.0)
    } else {
        format!("{}{:.2}", sign, abs)
    }
}

/// Format a crypto price with appropriate decimal places.
/// High-value assets (>$1) get 2 decimals; sub-dollar get more precision.
fn format_price(price: f64) -> String {
    if price >= 1.0 {
        format!("{:.2}", price)
    } else if price >= 0.01 {
        format!("{:.4}", price)
    } else if price >= 0.0001 {
        format!("{:.6}", price)
    } else {
        format!("{:.8}", price)
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct FinanceDeps {
    http_client: reqwest::Client,
}

static FINANCE_DEPS: OnceLock<FinanceDeps> = OnceLock::new();

/// Initialize finance server dependencies. Call once at startup.
///
/// No settings repository: the Finnhub key comes from the secret store,
/// installed separately by `init_secret_deps` (PAI-2 P2).
pub fn init_finance_deps(http_client: reqwest::Client) {
    let _ = FINANCE_DEPS.set(FinanceDeps { http_client });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_finance_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = FINANCE_DEPS.get().expect("init_finance_deps() not called");
    let server = FinanceMcpServer::new(deps.http_client.clone());
    crate::serve_builtin("giap-finance", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currency_code_normalizes_to_uppercase() {
        assert_eq!(normalize_currency_code("usd"), "USD");
        assert_eq!(normalize_currency_code("eur"), "EUR");
        assert_eq!(normalize_currency_code("  gbp  "), "GBP");
        assert_eq!(normalize_currency_code("Jpy"), "JPY");
    }

    #[test]
    fn default_base_currency_is_usd() {
        let result = resolve_currency_code(None, &Default::default(), &["base"]);
        assert!(result.is_none());
        // The tool implementation defaults to "USD" when resolve returns None
    }

    #[test]
    fn resolve_currency_from_extras() {
        let mut extras = std::collections::HashMap::new();
        extras.insert(
            "base".to_string(),
            serde_json::Value::String("eur".to_string()),
        );
        let result = resolve_currency_code(None, &extras, &["base", "currency"]);
        assert_eq!(result, Some("EUR".to_string()));
    }

    #[test]
    fn crypto_symbol_maps_to_coingecko_id() {
        assert_eq!(crypto_symbol_to_coingecko_id("BTC"), "bitcoin");
        assert_eq!(crypto_symbol_to_coingecko_id("btc"), "bitcoin");
        assert_eq!(crypto_symbol_to_coingecko_id("ETH"), "ethereum");
        assert_eq!(crypto_symbol_to_coingecko_id("ethereum"), "ethereum");
        assert_eq!(crypto_symbol_to_coingecko_id("SOL"), "solana");
        assert_eq!(crypto_symbol_to_coingecko_id("DOGE"), "dogecoin");
        assert_eq!(crypto_symbol_to_coingecko_id("XRP"), "ripple");
        assert_eq!(crypto_symbol_to_coingecko_id("BNB"), "binancecoin");
        assert_eq!(crypto_symbol_to_coingecko_id("USDT"), "tether");
        assert_eq!(crypto_symbol_to_coingecko_id("ADA"), "cardano");
    }

    #[test]
    fn crypto_unknown_lowercases_and_hyphenates() {
        assert_eq!(crypto_symbol_to_coingecko_id("some coin"), "some-coin");
        assert_eq!(crypto_symbol_to_coingecko_id("MY TOKEN"), "my-token");
    }

    #[test]
    fn format_large_number_works() {
        assert_eq!(format_large_number(1_500_000_000_000.0), "1.50T");
        assert_eq!(format_large_number(1_500_000_000.0), "1.50B");
        assert_eq!(format_large_number(1_500_000.0), "1.50M");
        assert_eq!(format_large_number(1_500.0), "1.50K");
        assert_eq!(format_large_number(150.0), "150.00");
        assert_eq!(format_large_number(0.0), "0.00");
    }

    #[test]
    fn format_large_number_handles_negatives() {
        assert_eq!(format_large_number(-1_500_000_000.0), "-1.50B");
        assert_eq!(format_large_number(-500.0), "-500.00");
    }

    #[test]
    fn format_large_number_handles_boundaries() {
        assert_eq!(format_large_number(1_000.0), "1.00K");
        assert_eq!(format_large_number(1_000_000.0), "1.00M");
        assert_eq!(format_large_number(1_000_000_000.0), "1.00B");
        assert_eq!(format_large_number(1_000_000_000_000.0), "1.00T");
    }

    #[test]
    fn stock_symbol_strips_dollar_prefix() {
        assert_eq!(normalize_stock_symbol("$AAPL"), "AAPL");
        assert_eq!(normalize_stock_symbol("$msft"), "MSFT");
        assert_eq!(normalize_stock_symbol("TSLA"), "TSLA");
        assert_eq!(normalize_stock_symbol("  $goog  "), "GOOG");
    }

    #[test]
    fn resolve_stock_symbol_from_extras() {
        let mut extras = std::collections::HashMap::new();
        extras.insert(
            "ticker".to_string(),
            serde_json::Value::String("$NVDA".to_string()),
        );
        let result = resolve_stock_symbol(None, &extras);
        assert_eq!(result, Some("NVDA".to_string()));
    }

    #[test]
    fn resolve_amount_defaults_to_one() {
        assert_eq!(resolve_amount(None, &Default::default()), 1.0);
    }

    #[test]
    fn resolve_amount_from_primary() {
        assert_eq!(resolve_amount(Some(100.0), &Default::default()), 100.0);
    }

    #[test]
    fn resolve_amount_from_extras() {
        let mut extras = std::collections::HashMap::new();
        extras.insert(
            "amount".to_string(),
            serde_json::Value::Number(serde_json::Number::from_f64(50.0).unwrap()),
        );
        assert_eq!(resolve_amount(None, &extras), 50.0);
    }

    #[test]
    fn resolve_amount_from_string_extra() {
        let mut extras = std::collections::HashMap::new();
        extras.insert(
            "value".to_string(),
            serde_json::Value::String("250.50".to_string()),
        );
        assert_eq!(resolve_amount(None, &extras), 250.50);
    }

    #[test]
    fn format_price_high_value() {
        assert_eq!(format_price(67234.12), "67234.12");
        assert_eq!(format_price(1.50), "1.50");
    }

    #[test]
    fn format_price_low_value() {
        assert_eq!(format_price(0.05), "0.0500");
        assert_eq!(format_price(0.001), "0.001000");
        assert_eq!(format_price(0.00001), "0.00001000");
    }

    #[test]
    fn resolve_crypto_asset_from_primary() {
        let result = resolve_crypto_asset(Some("BTC"), &Default::default());
        assert_eq!(result, Some("bitcoin".to_string()));
    }

    #[test]
    fn resolve_crypto_asset_from_extras() {
        let mut extras = std::collections::HashMap::new();
        extras.insert(
            "coin".to_string(),
            serde_json::Value::String("ETH".to_string()),
        );
        let result = resolve_crypto_asset(None, &extras);
        assert_eq!(result, Some("ethereum".to_string()));
    }

    #[test]
    fn resolve_crypto_asset_none_when_empty() {
        let result = resolve_crypto_asset(None, &Default::default());
        assert!(result.is_none());
    }

    // ── Live integration tests ───────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_frankfurter_exchange_rate() {
        let client = reqwest::Client::new();
        let url = format!("{}/latest?base=USD&symbols=EUR", FRANKFURTER_BASE_URL);
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("Frankfurter request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp
            .json()
            .await
            .expect("failed to parse Frankfurter response");
        let eur_rate = body["rates"]["EUR"].as_f64().expect("EUR rate missing");
        eprintln!("USD->EUR rate: {}", eur_rate);
        assert!(
            (0.5..2.0).contains(&eur_rate),
            "EUR rate should be between 0.5 and 2.0, got {}",
            eur_rate,
        );
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_coingecko_bitcoin_price() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/0.1 (GIAP MCP)")
            .build()
            .unwrap();
        let url = format!(
            "{}/simple/price?ids=bitcoin&vs_currencies=usd&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true",
            COINGECKO_BASE_URL,
        );
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("CoinGecko request failed");
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "CoinGecko returned HTTP {} — body: {}",
            status,
            &body_text[..body_text.len().min(200)],
        );
        let body: serde_json::Value =
            serde_json::from_str(&body_text).expect("failed to parse CoinGecko response");
        let price = body["bitcoin"]["usd"].as_f64().expect("usd price missing");
        eprintln!("Bitcoin price: ${}", price);
        assert!(price > 0.0, "Bitcoin price should be non-zero");
        let change = body["bitcoin"]["usd_24h_change"].as_f64();
        eprintln!("24h change: {:?}%", change);
    }

    #[tokio::test]
    #[ignore] // requires internet + GIAP_FINNHUB_KEY env var
    async fn live_finnhub_stock_quote() {
        let key = match std::env::var("GIAP_FINNHUB_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skipping: GIAP_FINNHUB_KEY not set");
                return;
            }
        };
        let client = reqwest::Client::new();
        let url = format!("{}/quote?symbol=AAPL&token={}", FINNHUB_BASE_URL, key,);
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("Finnhub request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.expect("failed to parse Finnhub response");
        let current = body["c"].as_f64().expect("current price missing");
        eprintln!("AAPL price: ${}", current);
        assert!(current > 0.0, "AAPL price should be non-zero");
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_yahoo_finance_stock_quote() {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; GIAP/0.1)")
            .build()
            .unwrap();
        let url = format!("{}/AAPL?interval=1d&range=1d", YAHOO_FINANCE_BASE_URL);
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("Yahoo Finance request failed");
        assert!(
            resp.status().is_success(),
            "Yahoo Finance returned HTTP {}",
            resp.status(),
        );
        let body: serde_json::Value = resp.json().await.expect("parse failed");
        let price = body["chart"]["result"][0]["meta"]["regularMarketPrice"]
            .as_f64()
            .expect("price missing");
        eprintln!("AAPL price: ${}", price);
        assert!(price > 0.0, "AAPL price should be non-zero");
    }
}
