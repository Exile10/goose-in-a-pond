//! `search_web` for `giap-knowledge`: one Brave search with the household's key, the top pages
//! read through a client that reaches only public addresses, and the passages that answer the
//! question. Brave's results are used transiently and never written to disk (its terms, 3(b)(i)).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use pond_core::mcp::domain::tool_group::BRAVE_SEARCH_API_KEY;
use pond_core::mcp::domain::web_search::{
    WebSearchStatus, BRAVE_API_BASE, BRAVE_TIMEOUT, BRAVE_TOKEN_HEADER, BRAVE_WEB_SEARCH_PATH,
};
use pond_core::mcp::services::web_search_status::{self, WebSearchStatusBoard};
use pond_core::shared::services::passages;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{Annotations, CallToolResult, ContentBlock, ErrorData, Role, TextContent},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::format::{format_api_error, format_dead_end, format_no_results};
use crate::knowledge::KnowledgeMcpServer;

// ── Constants ──────────────────────────────────────────────────────────────

/// Results asked of Brave.
const RESULT_COUNT: usize = 8;

/// Brave's own limits on `q`.
const BRAVE_MAX_QUERY_CHARS: usize = 400;
const BRAVE_MAX_QUERY_WORDS: usize = 50;

/// How many of the top results have their page read.
const PAGES_READ: usize = 3;

/// One page, connect to last byte.
const PAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// All the pages together; a page still loading then falls back to its snippets.
const PAGES_DEADLINE: Duration = Duration::from_secs(7);

/// A page is read up to this many bytes and parsed from what was read.
const PAGE_BYTE_CAP: usize = 1_500_000;

/// Redirects a page may take.
const PAGE_MAX_REDIRECTS: usize = 3;

/// Text blocks kept per page, so a huge page cannot dominate the ranking's cost.
const MAX_BLOCKS_PER_PAGE: usize = 400;

/// A non-heading block shorter than this is navigation, a caption or a button.
const MIN_BLOCK_CHARS: usize = 40;

/// The model's copy, whole, in bytes: about 450 tokens.
const RESULT_BUDGET: usize = 1_800;

/// Passages quoted from one source, so one site cannot fill the answer.
const MAX_PASSAGES_PER_SOURCE: usize = 2;

/// One passage, in bytes, before it is placed.
const MAX_PASSAGE_BYTES: usize = 600;

/// The question as echoed at the top of the model's copy.
const MAX_QUESTION_ECHO_BYTES: usize = 200;

const MAX_TITLE_BYTES: usize = 120;
const MAX_AGE_BYTES: usize = 40;

/// The card's snippet per result.
const MAX_SNIPPET_BYTES: usize = 300;

/// The tool this module serves, as the egress record attributes it.
const TOOL_NAME: &str = "search_web";

const PAGE_USER_AGENT: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";

/// Elements whose whole subtree is boilerplate or not text.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "iframe", "nav", "header", "footer", "aside",
    "form",
];

/// Elements whose text is a passage.
const BLOCKS: &[&str] = &[
    "p",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "td",
    "th",
    "blockquote",
    "pre",
    "dd",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4"];

/// Elements that run inside a line, so their text joins without a space.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "font", "i", "kbd",
    "mark", "q", "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr",
];

// ── Parameters ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchWebParams {
    /// The user's question, in their own words.
    // Advertised as a required string; deserialised leniently so `extra` can rescue an old name.
    #[schemars(with = "String")]
    pub question: Option<String>,
    /// Catch-all for the names a model may remember instead ("query", "q", "topic").
    /// Not part of the advertised schema.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

/// The question, from `question` or a remembered name for it.
fn resolve_question(params: &SearchWebParams) -> String {
    let named = params.question.as_deref().map(str::trim);
    if let Some(q) = named.filter(|q| !q.is_empty()) {
        return q.to_string();
    }
    ["query", "q", "topic"]
        .iter()
        .filter_map(|key| params.extra.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|q| !q.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
}

// ── The tool ───────────────────────────────────────────────────────────────

#[tool_router(router = web_search_tool_router, vis = "pub")]
impl KnowledgeMcpServer {
    #[tool(description = "\
Search the web for anything current, local or specific that the encyclopedia will not have: \
news, opening hours, prices, products, businesses, recent results.")]
    async fn search_web(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SearchWebParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool(TOOL_NAME);
        let question = resolve_question(&params.0);
        if question.is_empty() {
            return Ok(text_result(
                "I need the user's question. Retry this tool with a 'question' parameter \
                 carrying it in their own words.",
            ));
        }
        // Re-checked here: the turn was offered the tool with a key the household may since
        // have removed.
        let Some(key) = brave_key().await else {
            return Ok(text_result(&key_removed()));
        };
        Ok(WebSearch {
            brave_base: BRAVE_API_BASE,
            brave: &self.http_client,
            pages: page_client(),
            board: web_search_status::global(),
        }
        .run(&key, &question)
        .await)
    }
}

async fn brave_key() -> Option<String> {
    crate::secrets::secret(BRAVE_SEARCH_API_KEY)
        .await
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// One client for every page fetch in this process; `None` if TLS could not initialise.
fn page_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            pond_net::public_only::client_builder(PAGE_MAX_REDIRECTS)
                .user_agent(PAGE_USER_AGENT)
                .build()
                .map_err(|e| {
                    tracing::warn!(
                        error = %e,
                        "search_web page client failed to build; answering from snippets"
                    )
                })
                .ok()
        })
        .as_ref()
}

fn text_result(text: &str) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text.to_string())])
}

// ── Misses (section 8.3 step 8) ────────────────────────────────────────────

fn key_rejected() -> String {
    format_dead_end(
        "a web search for this question",
        "The web search key was rejected.",
    )
}

fn allowance_used_up() -> String {
    format_dead_end(
        "a web search for this question",
        "This month's web search allowance is used up.",
    )
}

fn key_removed() -> String {
    format_dead_end(
        "a web search for this question",
        "Web search is not set up on this pond.",
    )
}

fn search_failed(why: &str) -> String {
    format_api_error("Web search", why)
}

fn nothing_found(question: &str) -> String {
    format_no_results(
        &format!("a web search for \"{}\"", echo(question)),
        &["giap-knowledge__get_wikipedia_article"],
    )
}

// ── The pipeline ───────────────────────────────────────────────────────────

/// One Brave result, as the model's copy and the card use it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WebHit {
    pub title: String,
    pub url: String,
    pub description: String,
    pub age: Option<String>,
    pub extra_snippets: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
    age: Option<String>,
    #[serde(default)]
    extra_snippets: Vec<String>,
}

/// One search, end to end. `brave_base` and both clients are injectable so tests hit stubs.
struct WebSearch<'a> {
    brave_base: &'a str,
    /// The ordinary pond client, for Brave's own host.
    brave: &'a reqwest::Client,
    /// The public-only client; `None` answers from snippets alone.
    pages: Option<&'a reqwest::Client>,
    board: &'a WebSearchStatusBoard,
}

impl WebSearch<'_> {
    async fn run(&self, key: &str, question: &str) -> CallToolResult {
        let hits = match self.search(key, question).await {
            Ok(hits) => hits,
            Err(miss) => return text_result(&miss),
        };
        if hits.is_empty() {
            return text_result(&nothing_found(question));
        }
        let bodies = match self.pages {
            Some(client) => fetch_pages(client, &hits).await,
            None => vec![None; hits.len()],
        };
        let pages = extract_off_thread(bodies).await;
        let result = result_from(question, &hits, &pages);
        tracing::info!(
            target: "giap::trace",
            kind = "web_search",
            results = hits.len(),
            pages_read = pages.iter().filter(|p| p.is_some()).count(),
            "web search answered"
        );
        result
    }

    /// Brave's results, or the model's text for a miss. Records what the answer said of the key.
    async fn search(&self, key: &str, question: &str) -> Result<Vec<WebHit>, String> {
        let url = brave_url(self.brave_base, question)
            .ok_or_else(|| search_failed("the search address could not be built"))?;
        let response = crate::http::traced_get_with(self.brave, url.as_str(), |b| {
            b.header(BRAVE_TOKEN_HEADER, key)
                .header(reqwest::header::ACCEPT, "application/json")
                .timeout(BRAVE_TIMEOUT)
        })
        .await
        .map_err(|e| search_failed(&describe_failure(&e)))?;

        let status = response.status().as_u16();
        let verdict = WebSearchStatus::from_brave_http(status);
        if let Some(verdict) = verdict {
            self.board.record(key, verdict);
        }
        match verdict {
            Some(WebSearchStatus::Rejected) => return Err(key_rejected()),
            Some(WebSearchStatus::Quota) => return Err(allowance_used_up()),
            Some(WebSearchStatus::Ok) => {}
            _ => return Err(search_failed(&format!("it answered HTTP {status}"))),
        }
        let body: BraveResponse = response
            .json()
            .await
            .map_err(|_| search_failed("its answer could not be read"))?;
        Ok(body
            .web
            .map(|w| w.results)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !r.url.trim().is_empty())
            .take(RESULT_COUNT)
            .map(|r| WebHit {
                title: plain_text(&r.title),
                url: r.url.trim().to_string(),
                description: plain_text(&r.description),
                age: r.age.map(|a| plain_text(&a)).filter(|a| !a.is_empty()),
                extra_snippets: r
                    .extra_snippets
                    .iter()
                    .map(|s| plain_text(s))
                    .filter(|s| !s.is_empty())
                    .collect(),
            })
            .collect())
    }
}

/// The search request, per section 8.3 step 1. No `country`: Kenya is not a documented value.
fn brave_url(base: &str, question: &str) -> Option<reqwest::Url> {
    let q = brave_query(question);
    let count = RESULT_COUNT.to_string();
    reqwest::Url::parse_with_params(
        &format!("{}{BRAVE_WEB_SEARCH_PATH}", base.trim_end_matches('/')),
        [
            ("q", q.as_str()),
            ("count", count.as_str()),
            ("extra_snippets", "true"),
            ("safesearch", "moderate"),
            ("text_decorations", "false"),
        ],
    )
    .ok()
}

/// The question within Brave's word and character limits.
fn brave_query(question: &str) -> String {
    let words: Vec<&str> = question
        .split_whitespace()
        .take(BRAVE_MAX_QUERY_WORDS)
        .collect();
    words
        .join(" ")
        .chars()
        .take(BRAVE_MAX_QUERY_CHARS)
        .collect()
}

/// A failure in words the model can pass on; never the URL, which carries the question.
fn describe_failure(error: &anyhow::Error) -> String {
    if let Some(denied) = error.downcast_ref::<pond_core::shared::services::egress::EgressDenied>()
    {
        return denied.to_string();
    }
    match error.downcast_ref::<reqwest::Error>() {
        Some(e) if e.is_timeout() => "it did not answer in time".to_string(),
        Some(e) if e.is_connect() => "it could not be reached".to_string(),
        _ => "the request failed".to_string(),
    }
}

/// The top [`PAGES_READ`] `http(s)` results' HTML, in hit order; `None` where a page was not
/// read. Concurrent, under one deadline, every request gated and recorded.
async fn fetch_pages(client: &reqwest::Client, hits: &[WebHit]) -> Vec<Option<String>> {
    let deadline = tokio::time::Instant::now() + PAGES_DEADLINE;
    let chosen: Vec<usize> = hits
        .iter()
        .enumerate()
        .filter(|(_, h)| h.url.starts_with("http://") || h.url.starts_with("https://"))
        .map(|(i, _)| i)
        .take(PAGES_READ)
        .collect();
    let fetches = chosen.iter().map(|&i| async move {
        let page = tokio::time::timeout_at(deadline, fetch_page(client, &hits[i].url))
            .await
            .ok()
            .flatten();
        (i, page)
    });
    let mut bodies = vec![None; hits.len()];
    for (i, page) in futures::future::join_all(fetches).await {
        bodies[i] = page;
    }
    bodies
}

/// One page's HTML, or `None` when it is refused, fails, is not HTML or is empty.
async fn fetch_page(client: &reqwest::Client, url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    if let Err(refused) = pond_net::public_only::check_target(&parsed) {
        tracing::info!(target: "giap::trace", kind = "web_page_refused", host = %host, %refused);
        return None;
    }
    let response = match crate::http::traced_get_with(client, url, |b| {
        b.timeout(PAGE_TIMEOUT)
            .header(reqwest::header::ACCEPT, "text/html")
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::info!(target: "giap::trace", kind = "web_page_unread", host = %host, error = %e);
            return None;
        }
    };
    if !response.status().is_success() || !is_html(&response) {
        return None;
    }
    let body = read_capped(response, PAGE_BYTE_CAP).await?;
    (!body.trim().is_empty()).then_some(body)
}

fn is_html(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
        })
}

/// At most `cap` bytes of the body, as text; the rest is never read.
async fn read_capped(mut response: reqwest::Response, cap: usize) -> Option<String> {
    let mut body: Vec<u8> = Vec::new();
    while body.len() < cap {
        let Some(chunk) = response.chunk().await.ok()? else {
            break;
        };
        let room = cap - body.len();
        body.extend_from_slice(&chunk[..room.min(chunk.len())]);
    }
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// Each page's text blocks, parsed one page at a time off the async workers.
async fn extract_off_thread(bodies: Vec<Option<String>>) -> Vec<Option<Vec<String>>> {
    let count = bodies.len();
    tokio::task::spawn_blocking(move || {
        bodies
            .into_iter()
            .map(|body| body.map(|html| extract_blocks(&html)))
            .map(|blocks| blocks.filter(|b| !b.is_empty()))
            .collect()
    })
    .await
    .unwrap_or_else(|_| vec![None; count])
}

// ── Extraction (section 8.3 step 4) ────────────────────────────────────────

/// The page's readable text blocks, in document order, boilerplate dropped.
pub fn extract_blocks(html: &str) -> Vec<String> {
    let document = scraper::Html::parse_document(html);
    let mut out = Blocks::default();
    // Iterative, so a deeply nested page cannot exhaust the stack.
    let mut stack = vec![(document.tree.root(), false)];
    while let Some((node, closing)) = stack.pop() {
        if out.full() {
            break;
        }
        match node.value() {
            scraper::Node::Element(element) => {
                let name = element.name();
                if closing {
                    out.close(name);
                    continue;
                }
                let hidden = element
                    .attr("aria-hidden")
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
                if DROPPED.contains(&name) || hidden {
                    continue;
                }
                out.open(name);
                stack.push((node, true));
            }
            scraper::Node::Text(text) => {
                out.text(text);
                continue;
            }
            _ => {}
        }
        let mut child = node.last_child();
        while let Some(c) = child {
            stack.push((c, false));
            child = c.prev_sibling();
        }
    }
    out.blocks
}

/// Text blocks as the walk finds them. A block nested in another ends the outer one's current
/// run, so no text is quoted twice.
#[derive(Default)]
struct Blocks {
    blocks: Vec<String>,
    /// Whether each open block element is a heading, innermost last.
    open: Vec<bool>,
    current: String,
}

impl Blocks {
    fn full(&self) -> bool {
        self.blocks.len() >= MAX_BLOCKS_PER_PAGE
    }

    fn open(&mut self, name: &str) {
        if BLOCKS.contains(&name) {
            self.flush();
            self.open.push(HEADINGS.contains(&name));
        } else if !self.open.is_empty() && !INLINE.contains(&name) {
            self.current.push(' ');
        }
    }

    fn close(&mut self, name: &str) {
        if BLOCKS.contains(&name) {
            self.flush();
            self.open.pop();
        } else if !self.open.is_empty() && !INLINE.contains(&name) {
            self.current.push(' ');
        }
    }

    fn text(&mut self, text: &str) {
        if !self.open.is_empty() {
            self.current.push_str(text);
        }
    }

    fn flush(&mut self) {
        let heading = self.open.last().copied().unwrap_or(false);
        let collapsed = self
            .current
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        self.current.clear();
        let long_enough = heading || collapsed.chars().count() >= MIN_BLOCK_CHARS;
        if !collapsed.is_empty() && long_enough && !self.blocks.contains(&collapsed) {
            self.blocks.push(collapsed);
        }
    }
}

// ── The result (section 8.3 steps 5 to 7) ──────────────────────────────────

/// The tool result for `hits`, with `pages[i]` the blocks read from hit `i`'s page (`None` if
/// unread): the model's copy, then the card in a block only the user's side reads.
pub fn result_from(
    question: &str,
    hits: &[WebHit],
    pages: &[Option<Vec<String>>],
) -> CallToolResult {
    let copy = model_copy(question, hits, pages).unwrap_or_else(|| nothing_found(question));
    CallToolResult::success(vec![ContentBlock::text(copy), card_block(question, hits)])
}

/// The plain-text answer for the model, within [`RESULT_BUDGET`]; `None` if nothing quotable.
fn model_copy(question: &str, hits: &[WebHit], pages: &[Option<Vec<String>>]) -> Option<String> {
    // A source whose page was read speaks with its page; only an unread one with Brave's text.
    let mut texts: Vec<String> = Vec::new();
    let mut source_of: Vec<usize> = Vec::new();
    for (i, hit) in hits.iter().enumerate() {
        let page = pages
            .get(i)
            .and_then(Option::as_ref)
            .filter(|b| !b.is_empty());
        let candidates: Vec<&String> = match page {
            Some(blocks) => blocks.iter().collect(),
            None => std::iter::once(&hit.description)
                .chain(&hit.extra_snippets)
                .collect(),
        };
        for text in candidates {
            let clean = without_urls(text);
            if !clean.is_empty() {
                texts.push(clean);
                source_of.push(i);
            }
        }
    }

    let ranked = passages::rank(question, &texts);
    let order: Vec<usize> = if ranked.iter().any(|r| r.score > 0.0) {
        ranked
            .iter()
            .filter(|r| r.score > 0.0)
            .map(|r| r.index)
            .collect()
    } else {
        ranked.iter().map(|r| r.index).collect()
    };

    let header = format!("Web results for: {}", echo(question));
    let mut used = header.len();
    let mut chosen: Vec<(usize, String)> = Vec::new();
    let mut per_source: HashMap<usize, usize> = HashMap::new();
    for index in order {
        let source = source_of[index];
        let quoted = per_source.get(&source).copied().unwrap_or(0);
        if quoted >= MAX_PASSAGES_PER_SOURCE {
            continue;
        }
        let passage = passages::clip(&texts[index], MAX_PASSAGE_BYTES);
        let mut cost = passage.len() + 1;
        if quoted == 0 {
            cost += source_line(&hits[source]).len() + 1;
        }
        if used + cost > RESULT_BUDGET {
            continue;
        }
        used += cost;
        per_source.insert(source, quoted + 1);
        chosen.push((index, passage));
    }
    if chosen.is_empty() {
        return None;
    }

    // Sources in search order, each one's passages in its own order.
    chosen.sort_by_key(|(index, _)| (source_of[*index], *index));
    let mut out = header;
    let mut last_source = None;
    for (index, passage) in chosen {
        let source = source_of[index];
        if last_source != Some(source) {
            out.push('\n');
            out.push_str(&source_line(&hits[source]));
            last_source = Some(source);
        }
        out.push('\n');
        out.push_str(&passage);
    }
    Some(out)
}

/// `Title (domain[, age])`.
fn source_line(hit: &WebHit) -> String {
    let domain = domain_of(&hit.url);
    let title = passages::clip(&without_urls(&hit.title), MAX_TITLE_BYTES);
    let title = if title.is_empty() {
        domain.clone()
    } else {
        title
    };
    match hit.age.as_deref() {
        Some(age) => format!("{title} ({domain}, {})", passages::clip(age, MAX_AGE_BYTES)),
        None => format!("{title} ({domain})"),
    }
}

/// The question as one line within its echo budget.
fn echo(question: &str) -> String {
    passages::clip(
        &question.split_whitespace().collect::<Vec<_>>().join(" "),
        MAX_QUESTION_ECHO_BYTES,
    )
}

/// The host a result lives on, without `www.`.
fn domain_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .map(|h| h.strip_prefix("www.").map(str::to_string).unwrap_or(h))
        .unwrap_or_default()
}

/// `text` without any URL in it: the model's copy carries none.
fn without_urls(text: &str) -> String {
    text.split_whitespace()
        .filter(|word| {
            let lower = word.to_ascii_lowercase();
            !(lower.contains("://") || lower.starts_with("www."))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tags out, the common entities decoded, whitespace collapsed: Brave's fields are plain text
/// with decorations off, and this keeps them so if one slips through.
fn plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = [
        ("&nbsp;", " "),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&#x27;", "'"),
        ("&apos;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
    ]
    .iter()
    .fold(out, |acc, (entity, ch)| acc.replace(entity, ch));
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `[[[mcp-ui:search:{...}]]]` card, annotated for the user only: goose drops such blocks
/// before the provider, so the model never reads it.
fn card_block(question: &str, hits: &[WebHit]) -> ContentBlock {
    let card = serde_json::json!({
        "query": sanitize(question),
        "results": hits.iter().map(|h| serde_json::json!({
            "title": sanitize(&h.title),
            "url": sanitize(&h.url),
            "domain": sanitize(&domain_of(&h.url)),
            "snippet": sanitize(&passages::clip(&h.description, MAX_SNIPPET_BYTES)),
        })).collect::<Vec<_>>(),
    });
    let marker = format!("[[[mcp-ui:search:{card}]]]");
    ContentBlock::Text(
        TextContent::new(marker)
            .with_annotations(Annotations::default().with_audience(vec![Role::User])),
    )
}

/// Neutralise `]]]`, which would end the card's marker early.
fn sanitize(s: &str) -> String {
    s.replace("]]]", "] ] ]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The encyclopedia tool a miss points at.
    const ENCYCLOPEDIA_TOOL: &str = "giap-knowledge__get_wikipedia_article";

    // ── Fixtures ──────────────────────────────────────────────────────────

    fn hit(title: &str, url: &str, description: &str) -> WebHit {
        WebHit {
            title: title.to_string(),
            url: url.to_string(),
            description: description.to_string(),
            age: None,
            extra_snippets: Vec::new(),
        }
    }

    fn text_of(result: &CallToolResult, block: usize) -> String {
        result.content[block]
            .as_text()
            .map(|t| t.text.clone())
            .expect("a text block")
    }

    /// What goose's agent-visible projection keeps: blocks with no audience, or the assistant's.
    fn agent_visible_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text(t) => {
                    let audience = t.annotations.as_ref().and_then(|a| a.audience.as_ref());
                    audience
                        .is_none_or(|roles| roles.contains(&Role::Assistant))
                        .then(|| t.text.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── The schema ────────────────────────────────────────────────────────

    #[test]
    fn the_schema_asks_for_one_required_question() {
        let tool = KnowledgeMcpServer::web_search_tool_router()
            .list_all()
            .into_iter()
            .find(|t| t.name == "search_web")
            .expect("search_web is routed");
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert_eq!(
            schema["required"],
            serde_json::json!(["question"]),
            "{schema}"
        );
        assert_eq!(
            schema["properties"]["question"]["type"], "string",
            "{schema}"
        );
        assert_eq!(
            schema["properties"]["question"]["description"],
            "The user's question, in their own words."
        );
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            properties.len(),
            1,
            "only `question` is advertised: {schema}"
        );
    }

    #[test]
    fn the_description_names_the_act_not_the_backend() {
        let tool = KnowledgeMcpServer::web_search_tool_router()
            .list_all()
            .into_iter()
            .find(|t| t.name == "search_web")
            .unwrap();
        let description = tool.description.unwrap_or_default().to_lowercase();
        assert!(description.starts_with("search the web"), "{description}");
        for vendor in ["brave", "google", "bing", "duckduckgo"] {
            assert!(!description.contains(vendor), "{description}");
        }
    }

    #[test]
    fn remembered_names_for_the_question_are_absorbed() {
        for key in ["query", "q", "topic"] {
            let params: SearchWebParams =
                serde_json::from_value(serde_json::json!({ key: " opening hours " })).unwrap();
            assert_eq!(resolve_question(&params), "opening hours", "{key}");
        }
        let params: SearchWebParams = serde_json::from_value(serde_json::json!({
            "question": "the real one", "query": "not this"
        }))
        .unwrap();
        assert_eq!(resolve_question(&params), "the real one");
        let params: SearchWebParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(resolve_question(&params), "");
    }

    #[test]
    fn the_query_keeps_within_braves_limits() {
        let long = "word ".repeat(80);
        assert_eq!(brave_query(&long).split_whitespace().count(), 50);
        let wide = "x".repeat(900);
        assert_eq!(brave_query(&wide).chars().count(), 400);
    }

    // ── Extraction ────────────────────────────────────────────────────────

    const PAGE: &str = r#"<!doctype html>
<html><head><title>Junction Mall</title><style>p { color: red; }</style>
<script>var secret = "this script text must never be quoted as a passage";</script></head>
<body>
<header><p>Header text that is long enough to count as a passage on its own.</p></header>
<nav><ul><li>A navigation item long enough to pass the forty character rule</li></ul></nav>
<main>
<h1>Opening hours</h1>
<p>Junction Mall is open   every day,
   from 8:30 in the morning to 9 in the evening.</p>
<p>Short line.</p>
<ul><li><p>Parking is free for the first two hours with any purchase.</p></li></ul>
<table><tr><th>Day</th><td>Monday to Saturday, 8:30 am to 9:00 pm, Sunday 10 am to 6 pm.</td></tr></table>
<div aria-hidden="true"><p>Hidden text that a screen reader skips and so do we, always.</p></div>
<blockquote>A visitor said the food court is the best in Nairobi by a long way.</blockquote>
<dl><dt>Address</dt><dd>Ngong Road, Nairobi, next to the Ngong Racecourse roundabout.</dd></dl>
<form><p>Subscribe to our newsletter for the latest offers and events.</p></form>
<aside><p>Related: the other malls in Nairobi you might like to visit.</p></aside>
<noscript><p>Please enable JavaScript to see the interactive map of the mall.</p></noscript>
<iframe src="x"></iframe><svg><text>Vector text is not a passage at all, ever.</text></svg>
<template><p>Template content is inert and must not be read as a passage.</p></template>
</main>
<footer><p>Copyright Junction Mall, all rights reserved, every single one.</p></footer>
</body></html>"#;

    #[test]
    fn boilerplate_is_dropped() {
        let blocks = extract_blocks(PAGE).join("\n");
        for gone in [
            "this script text",
            "color: red",
            "Header text",
            "navigation item",
            "Hidden text",
            "Subscribe",
            "Related:",
            "enable JavaScript",
            "Vector text",
            "Template content",
            "Copyright",
        ] {
            assert!(!blocks.contains(gone), "{gone:?} survived:\n{blocks}");
        }
    }

    #[test]
    fn text_blocks_are_kept_in_order_with_whitespace_collapsed() {
        let blocks = extract_blocks(PAGE);
        assert_eq!(
            blocks,
            vec![
                "Opening hours".to_string(),
                "Junction Mall is open every day, from 8:30 in the morning to 9 in the evening."
                    .to_string(),
                "Parking is free for the first two hours with any purchase.".to_string(),
                "Monday to Saturday, 8:30 am to 9:00 pm, Sunday 10 am to 6 pm.".to_string(),
                "A visitor said the food court is the best in Nairobi by a long way.".to_string(),
                "Ngong Road, Nairobi, next to the Ngong Racecourse roundabout.".to_string(),
            ],
            "headings stay whatever their length; short non-headings and `th` go"
        );
    }

    #[test]
    fn inline_markup_joins_and_block_markup_separates() {
        let blocks = extract_blocks(
            "<p>Ng<b>ũ</b>g<i>ĩ</i> wa Thiong'o wrote <a href='/x'>Petals of Blood</a> in 1977.</p>\
             <table><tr><td><div>First cell line of the table here</div>\
             <div>second line of it</div></td></tr></table>",
        );
        assert_eq!(
            blocks[0],
            "Ngũgĩ wa Thiong'o wrote Petals of Blood in 1977."
        );
        assert_eq!(
            blocks[1],
            "First cell line of the table here second line of it"
        );
    }

    #[test]
    fn a_deep_page_does_not_exhaust_the_stack() {
        let depth = 5_000;
        let html = format!(
            "{}<p>{}</p>{}",
            "<div>".repeat(depth),
            "deep text that is long enough to be a passage on its own",
            "</div>".repeat(depth)
        );
        let blocks = extract_blocks(&html);
        assert!(!blocks.is_empty());
    }

    // ── The model's copy ──────────────────────────────────────────────────

    fn junction_hits() -> Vec<WebHit> {
        vec![
            WebHit {
                age: Some("2 days ago".to_string()),
                ..hit(
                    "Junction Mall | Opening Hours",
                    "https://www.junction.co.ke/hours",
                    "BRAVE DESCRIPTION OF THE READ PAGE, which must not be quoted.",
                )
            },
            WebHit {
                extra_snippets: vec![
                    "The mall closes at 9pm on weekdays, see https://example.com/x for more."
                        .to_string(),
                ],
                ..hit(
                    "Malls in Nairobi",
                    "https://nairobimalls.example/junction",
                    "Junction Mall opens at 8am daily and closes late.",
                )
            },
        ]
    }

    fn junction_pages() -> Vec<Option<Vec<String>>> {
        vec![
            Some(vec![
                "Opening hours".to_string(),
                "Junction Mall is open every day from 8:30 am to 9:00 pm.".to_string(),
                "Parking is free for two hours with any purchase at the mall.".to_string(),
            ]),
            None,
        ]
    }

    #[test]
    fn the_copy_is_plain_text_by_source() {
        let copy = model_copy(
            "What time does Junction Mall close?",
            &junction_hits(),
            &junction_pages(),
        )
        .unwrap();
        let lines: Vec<&str> = copy.lines().collect();
        assert_eq!(
            lines[0],
            "Web results for: What time does Junction Mall close?"
        );
        assert_eq!(
            lines[1],
            "Junction Mall | Opening Hours (junction.co.ke, 2 days ago)"
        );
        assert!(copy.contains("8:30 am to 9:00 pm"), "{copy}");
        assert!(
            copy.contains("Malls in Nairobi (nairobimalls.example)"),
            "{copy}"
        );
        assert!(copy.contains("closes at 9pm"), "{copy}");
    }

    #[test]
    fn a_read_page_speaks_for_itself_and_not_with_braves_words() {
        let copy = model_copy(
            "What time does Junction Mall close?",
            &junction_hits(),
            &junction_pages(),
        )
        .unwrap();
        assert!(!copy.contains("BRAVE DESCRIPTION"), "{copy}");
        // The unread source falls back to Brave's description and extra snippets.
        let unread = model_copy("junction mall opens", &junction_hits(), &[None, None]).unwrap();
        assert!(unread.contains("opens at 8am"), "{unread}");
    }

    #[test]
    fn the_copy_has_no_url_and_no_markdown() {
        let copy = model_copy(
            "What time does Junction Mall close?",
            &junction_hits(),
            &junction_pages(),
        )
        .unwrap();
        assert!(!copy.contains("://"), "{copy}");
        assert!(!copy.contains("www."), "{copy}");
        assert!(!copy.contains("**") && !copy.contains("]("), "{copy}");
        for line in copy.lines() {
            for markdown in ["#", "- ", "* ", "> "] {
                assert!(!line.starts_with(markdown), "{line:?}");
            }
        }
        assert!(!copy.contains("mcp-ui"), "{copy}");
    }

    #[test]
    fn the_copy_holds_its_budget_and_two_passages_per_source() {
        let long = |n: usize| {
            format!(
                "Passage {n}: Junction Mall opening hours run long into the evening, {}",
                "and the shops stay open late. ".repeat(12)
            )
        };
        let hits: Vec<WebHit> = (0..8)
            .map(|i| {
                hit(
                    &format!("Source {i}"),
                    &format!("https://s{i}.example/"),
                    "",
                )
            })
            .collect();
        let pages: Vec<Option<Vec<String>>> = (0..8)
            .map(|i| Some((0..6).map(|j| long(i * 10 + j)).collect()))
            .collect();
        let copy = model_copy("Junction Mall opening hours", &hits, &pages).unwrap();
        assert!(copy.len() <= RESULT_BUDGET, "{} bytes", copy.len());
        for i in 0..8 {
            let from_source = (0..6)
                .filter(|j| copy.contains(&format!("Passage {}:", i * 10 + j)))
                .count();
            assert!(
                from_source <= MAX_PASSAGES_PER_SOURCE,
                "source {i}: {from_source}"
            );
        }
    }

    #[test]
    fn nothing_quotable_is_a_miss_that_offers_the_encyclopedia() {
        let hits = vec![hit("", "https://empty.example/", "")];
        let result = result_from("anything at all", &hits, &[None]);
        let copy = text_of(&result, 0);
        assert!(copy.starts_with("No results for"), "{copy}");
        assert!(copy.contains(ENCYCLOPEDIA_TOOL), "{copy}");
    }

    // ── The card ──────────────────────────────────────────────────────────

    #[test]
    fn the_card_rides_in_its_own_block_for_the_user_only() {
        let result = result_from(
            "What time does Junction Mall close?",
            &junction_hits(),
            &junction_pages(),
        );
        assert_eq!(result.content.len(), 2);
        assert!(!text_of(&result, 0).contains("mcp-ui"));

        let ContentBlock::Text(card) = &result.content[1] else {
            panic!("the card is a text block");
        };
        let audience = card.annotations.as_ref().and_then(|a| a.audience.clone());
        assert_eq!(audience, Some(vec![Role::User]));
        let json = card
            .text
            .strip_prefix("[[[mcp-ui:search:")
            .and_then(|rest| rest.strip_suffix("]]]"))
            .expect("the marker wraps the card");
        let data: Value = serde_json::from_str(json).unwrap();
        assert_eq!(data["query"], "What time does Junction Mall close?");
        assert_eq!(
            data["results"][0]["url"],
            "https://www.junction.co.ke/hours"
        );
        assert_eq!(data["results"][0]["domain"], "junction.co.ke");
        assert_eq!(data["results"][0]["title"], "Junction Mall | Opening Hours");
        assert!(data["results"][1]["snippet"]
            .as_str()
            .unwrap()
            .contains("opens at 8am"));
    }

    #[test]
    fn the_agent_visible_projection_has_no_card() {
        let result = result_from(
            "What time does Junction Mall close?",
            &junction_hits(),
            &junction_pages(),
        );
        let visible = agent_visible_text(&result);
        assert!(!visible.contains("mcp-ui"), "{visible}");
        assert!(visible.starts_with("Web results for:"), "{visible}");
    }

    #[test]
    fn a_title_cannot_end_the_marker_early() {
        let hits = vec![hit("Evil ]]] title", "https://e.example/", "desc ]]] here")];
        let ContentBlock::Text(card) = card_block("q", &hits) else {
            unreachable!()
        };
        assert_eq!(card.text.matches("]]]").count(), 1, "{}", card.text);
    }

    // ── Brave, against a stub ─────────────────────────────────────────────

    /// A loopback server answering `routes[path]` (status, content type, body) to each request,
    /// for up to `requests` connections; the request heads come back on the channel.
    fn stub(
        routes: Vec<(&'static str, u16, &'static str, String)>,
        requests: usize,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 16 * 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let _ = tx.send(head);
                let (status, content_type, body) = routes
                    .iter()
                    .find(|(p, ..)| *p == path)
                    .map(|(_, s, ct, b)| (*s, *ct, b.clone()))
                    .unwrap_or((404, "text/plain", "not here".to_string()));
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (port, rx)
    }

    fn brave_body(results: &[(&str, &str, &str)]) -> String {
        serde_json::json!({
            "type": "search",
            "web": {"results": results.iter().map(|(title, url, description)| serde_json::json!({
                "title": title, "url": url, "description": description,
                "age": "October 14, 2026", "extra_snippets": ["An extra snippet from Brave."],
            })).collect::<Vec<_>>()},
        })
        .to_string()
    }

    fn fresh_board() -> &'static WebSearchStatusBoard {
        Box::leak(Box::new(WebSearchStatusBoard::new()))
    }

    async fn search_against(
        brave_port: u16,
        pages: Option<&reqwest::Client>,
        board: &'static WebSearchStatusBoard,
    ) -> CallToolResult {
        let brave = crate::build_http_client();
        WebSearch {
            brave_base: &format!("http://127.0.0.1:{brave_port}"),
            brave: &brave,
            pages,
            board,
        }
        .run("test-key-123", "What time does Junction Mall close?")
        .await
    }

    #[tokio::test]
    async fn the_request_is_the_one_the_contract_names() {
        let body = brave_body(&[]);
        let (port, requests) = stub(
            vec![("/res/v1/web/search", 200, "application/json", body)],
            1,
        );
        let _ = search_against(port, None, fresh_board()).await;
        let head = requests.recv().unwrap();
        let line = head.lines().next().unwrap();
        assert!(line.starts_with("GET /res/v1/web/search?"), "{line}");
        for param in [
            "q=What+time+does+Junction+Mall+close%3F",
            "count=8",
            "extra_snippets=true",
            "safesearch=moderate",
            "text_decorations=false",
        ] {
            assert!(line.contains(param), "{param} missing: {line}");
        }
        assert!(!line.contains("country"), "{line}");
        let lower = head.to_ascii_lowercase();
        assert!(
            lower.contains("x-subscription-token: test-key-123"),
            "{head}"
        );
        assert!(lower.contains("accept: application/json"), "{head}");
    }

    #[tokio::test]
    async fn each_brave_status_maps_as_the_contract_says() {
        let cases: [(u16, &str, Option<WebSearchStatus>); 6] = [
            (
                401,
                "The web search key was rejected.",
                Some(WebSearchStatus::Rejected),
            ),
            (
                403,
                "The web search key was rejected.",
                Some(WebSearchStatus::Rejected),
            ),
            (402, "allowance is used up", Some(WebSearchStatus::Quota)),
            (429, "allowance is used up", Some(WebSearchStatus::Quota)),
            (500, "Web search is temporarily unavailable", None),
            (422, "Web search is temporarily unavailable", None),
        ];
        for (status, told, recorded) in cases {
            let (port, _rx) = stub(
                vec![(
                    "/res/v1/web/search",
                    status,
                    "application/json",
                    "{}".to_string(),
                )],
                1,
            );
            let board = fresh_board();
            let result = search_against(port, None, board).await;
            let copy = text_of(&result, 0);
            assert!(copy.contains(told), "{status}: {copy}");
            assert!(
                !copy.contains("test-key-123"),
                "{status}: the key reached the model"
            );
            assert_eq!(result.content.len(), 1, "{status}: a miss carries no card");
            let expected = recorded.unwrap_or(WebSearchStatus::Ok);
            assert_eq!(board.status_for(Some("test-key-123")), expected, "{status}");
        }
    }

    #[tokio::test]
    async fn an_unreachable_brave_is_an_api_error_that_names_no_url() {
        let board = fresh_board();
        let brave = crate::build_http_client();
        let result = WebSearch {
            brave_base: "http://127.0.0.1:1",
            brave: &brave,
            pages: None,
            board,
        }
        .run("test-key-123", "junction mall hours")
        .await;
        let copy = text_of(&result, 0);
        assert!(
            copy.contains("Web search is temporarily unavailable"),
            "{copy}"
        );
        assert!(!copy.contains("://"), "{copy}");
        assert_eq!(board.status_for(Some("test-key-123")), WebSearchStatus::Ok);
    }

    #[tokio::test]
    async fn no_results_offers_the_encyclopedia() {
        let (port, _rx) = stub(
            vec![(
                "/res/v1/web/search",
                200,
                "application/json",
                brave_body(&[]),
            )],
            1,
        );
        let board = fresh_board();
        let copy = text_of(&search_against(port, None, board).await, 0);
        assert!(copy.starts_with("No results for"), "{copy}");
        assert!(copy.contains(ENCYCLOPEDIA_TOOL), "{copy}");
        assert_eq!(board.status_for(Some("test-key-123")), WebSearchStatus::Ok);
    }

    #[tokio::test]
    async fn an_unreadable_answer_is_an_api_error() {
        let (port, _rx) = stub(
            vec![(
                "/res/v1/web/search",
                200,
                "application/json",
                "not json".to_string(),
            )],
            1,
        );
        let copy = text_of(&search_against(port, None, fresh_board()).await, 0);
        assert!(copy.contains("could not be read"), "{copy}");
    }

    #[tokio::test]
    async fn results_whose_pages_are_refused_answer_from_snippets() {
        // The real page client: a loopback result must be refused, never fetched.
        let (page_port, page_requests) = stub(vec![("/", 200, "text/html", PAGE.to_string())], 1);
        let body = brave_body(&[(
            "Junction Mall",
            &format!("http://127.0.0.1:{page_port}/"),
            "Junction Mall closes at 9pm, said Brave.",
        )]);
        let (port, _rx) = stub(
            vec![("/res/v1/web/search", 200, "application/json", body)],
            1,
        );
        let pages = page_client().expect("the page client builds");
        let result = search_against(port, Some(pages), fresh_board()).await;
        let copy = text_of(&result, 0);
        assert!(copy.contains("said Brave"), "{copy}");
        assert!(
            page_requests
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "a loopback page was fetched"
        );
    }

    /// A page client that treats `page.test` as a public site on the loopback stub.
    fn page_client_for(port: u16) -> reqwest::Client {
        pond_net::public_only::client_builder(PAGE_MAX_REDIRECTS)
            .resolve(
                "page.test",
                std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            )
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn read_pages_are_ranked_and_quoted() {
        let big = format!(
            "<p>Junction Mall closes at 9:00 pm every day of the week.</p>{}",
            "<p>Filler paragraph about something else entirely, repeated.</p>".repeat(40_000)
        );
        assert!(big.len() > PAGE_BYTE_CAP, "the fixture must exceed the cap");
        let (page_port, _pages_rx) = stub(
            vec![
                ("/hours", 200, "text/html; charset=utf-8", PAGE.to_string()),
                ("/pdf", 200, "application/pdf", "%PDF-1.7".to_string()),
                ("/big", 200, "text/html", big),
            ],
            3,
        );
        let page = |path: &str| format!("http://page.test:{page_port}{path}");
        let body = brave_body(&[
            (
                "Junction Mall hours",
                &page("/hours"),
                "BRAVE TEXT FOR THE HOURS PAGE.",
            ),
            (
                "A PDF",
                &page("/pdf"),
                "Junction Mall closes at 10pm on Fridays, says the PDF listing.",
            ),
            ("A big page", &page("/big"), "BRAVE TEXT FOR THE BIG PAGE."),
        ]);
        let (port, _rx) = stub(
            vec![("/res/v1/web/search", 200, "application/json", body)],
            1,
        );
        let pages = page_client_for(page_port);
        let result = search_against(port, Some(&pages), fresh_board()).await;
        let copy = text_of(&result, 0);
        assert!(copy.contains("8:30 am to 9:00 pm"), "{copy}");
        assert!(!copy.contains("BRAVE TEXT FOR THE HOURS PAGE"), "{copy}");
        assert!(
            copy.contains("says the PDF listing"),
            "an unread page: {copy}"
        );
        assert!(
            copy.contains("closes at 9:00 pm every day"),
            "the capped page: {copy}"
        );
        assert!(!copy.contains("BRAVE TEXT FOR THE BIG PAGE"), "{copy}");
        assert!(copy.len() <= RESULT_BUDGET);
        assert_eq!(result.content.len(), 2, "the card rides along");
    }

    // ── The MCP surface ───────────────────────────────────────────────────

    #[tokio::test]
    async fn with_no_key_stored_the_tool_says_web_search_is_not_set_up() {
        use rmcp::handler::server::ServerHandler;
        use rmcp::model::{CallToolRequestParams, RequestId};

        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let (_client, server_stream) = tokio::io::duplex(64);
        let running = rmcp::service::serve_directly(
            KnowledgeMcpServer::new(crate::build_http_client()),
            server_stream,
            None,
        );
        let ctx = RequestContext::new(RequestId::Number(0), running.peer().clone());
        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "search_web",
            "arguments": {"question": "what time does junction mall close"},
        }))
        .unwrap();
        // No secret store in a test binary: the key-removed path.
        let result = server.call_tool(params, ctx).await.unwrap();
        let copy = text_of(&result, 0);
        assert!(copy.contains("Web search is not set up"), "{copy}");
    }

    /// Run by hand with a real key: `BRAVE_SEARCH_API_KEY=... cargo test -p pond-mcp-server
    /// live_brave -- --ignored --nocapture`. Spends one search of the month's allowance.
    #[tokio::test]
    #[ignore]
    async fn live_brave_search_answers_a_local_question() {
        let key = std::env::var(BRAVE_SEARCH_API_KEY).expect("set BRAVE_SEARCH_API_KEY");
        let brave = crate::build_http_client();
        let board = fresh_board();
        let result = WebSearch {
            brave_base: BRAVE_API_BASE,
            brave: &brave,
            pages: page_client(),
            board,
        }
        .run(&key, "What time does Junction Mall Nairobi close?")
        .await;
        let copy = text_of(&result, 0);
        eprintln!("{copy}");
        assert!(copy.starts_with("Web results for:"), "{copy}");
        assert_eq!(board.status_for(Some(&key)), WebSearchStatus::Ok);
        assert_eq!(result.content.len(), 2);
    }
}
