//! `web_search` — provider-agnostic web search.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_http::Request;
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::time::Duration;

pub struct WebSearchTool;

/// Provider endpoint, overridable per provider through
/// `RIGHTKIT_WEB_SEARCH_<PROVIDER>_URL` (a mirror, or a local fake in offline
/// qualification). Unset or non-http(s) values fall back to the real service.
#[cfg(not(feature = "coding_tools-host"))]
fn provider_endpoint(_provider: &str, default: &str) -> String {
    default.to_string()
}

#[cfg(feature = "coding_tools-host")]
fn provider_endpoint(provider: &str, default: &str) -> String {
    std::env::var(format!(
        "RIGHTKIT_WEB_SEARCH_{}_URL",
        provider.to_ascii_uppercase()
    ))
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
    .unwrap_or_else(|| default.to_string())
}

/// Configuration for the web search provider.
#[derive(Debug, Clone)]
pub struct WebSearchConfig {
    pub provider: String,
    pub api_key: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub snippet: String,
    pub url: String,
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Search the web via keyless DuckDuckGo by default, or configured Brave/Tavily/Exa/TinyFish (default 10 results, max 20). Returns title+snippet+url."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "count": { "type": "integer", "description": "Number of results to return (default: 10)" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let query = match args["query"].as_str() {
            Some(q) => q.to_string(),
            None => return ToolResult::error("Missing required argument: query"),
        };
        let count = args["count"].as_u64().unwrap_or(10).min(20) as usize;

        let config = ctx.web_search_config.clone().unwrap_or(WebSearchConfig {
            provider: "duckduckgo".to_string(),
            api_key: String::new(),
        });

        let results = match config.provider.as_str() {
            "duckduckgo" => search_duckduckgo(&query, count).await,
            "brave" => search_brave(&query, count, &config.api_key).await,
            "tavily" => search_tavily(&query, count, &config.api_key).await,
            "exa" => search_exa(&query, count, &config.api_key).await,
            "tinyfish" => match crate::coding_tools::tinyfish::resolve_api_key(Some(&config.api_key)) {
                Ok(key) => crate::coding_tools::tinyfish::search(&query, count, &key).await,
                Err(e) => return ToolResult::error(e),
            },
            other => {
                return ToolResult::error(format!(
                    "Unknown web_search provider '{}'. Supported: duckduckgo, brave, tavily, exa, tinyfish.",
                    other
                ))
            }
        };

        match results {
            Ok(items) => {
                let content = serde_json::to_string_pretty(&items).unwrap_or_default();
                ToolResult::ok(crate::coding_tools::browser_fetch::wrap_untrusted_web_text(
                    "web_search",
                    &content,
                ))
            }
            Err(e) => ToolResult::error(format!("Search failed: {}", e)),
        }
    }
}

async fn search_duckduckgo(query: &str, count: usize) -> Result<Vec<SearchResult>, String> {
    let endpoint = provider_endpoint("duckduckgo", "https://html.duckduckgo.com/html/");
    let client =
        crate::coding_tools::http_client::client_following(&endpoint, Duration::from_secs(30))?;
    let request = Request::post(endpoint)
        .header("User-Agent", "Agent/0.1 web_search")
        .form(&[("q", query)]);
    let response = crate::coding_tools::http_client::send_any(&client, &request).await?;
    if !crate::coding_tools::http_client::is_success(&response) {
        return Err(format!("DuckDuckGo search HTTP {}", response.status));
    }
    parse_duckduckgo_html(&response.text(), count)
}

fn parse_duckduckgo_html(html: &str, count: usize) -> Result<Vec<SearchResult>, String> {
    let document = Html::parse_document(html);
    let result_selector = Selector::parse(".result").map_err(|error| error.to_string())?;
    let title_selector = Selector::parse(".result__a").map_err(|error| error.to_string())?;
    let snippet_selector =
        Selector::parse(".result__snippet").map_err(|error| error.to_string())?;
    let mut results = Vec::new();
    for result in document.select(&result_selector).take(count) {
        let Some(link) = result.select(&title_selector).next() else {
            continue;
        };
        let href = link.value().attr("href").unwrap_or_default();
        let url = duckduckgo_result_url(href);
        if url.is_empty() {
            continue;
        }
        let title = link.text().collect::<Vec<_>>().join(" ");
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|node| node.text().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        results.push(SearchResult {
            title: sanitize_search_field(title.trim()),
            snippet: sanitize_search_field(snippet.trim()),
            url,
        });
    }
    if results.is_empty() {
        return Err("DuckDuckGo returned no parseable search results".to_string());
    }
    Ok(results)
}

fn duckduckgo_result_url(href: &str) -> String {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };
    url::Url::parse(&absolute)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "uddg")
                .map(|(_, value)| value.into_owned())
                .or_else(|| Some(url.to_string()))
        })
        .unwrap_or_default()
}

async fn search_brave(
    query: &str,
    count: usize,
    api_key: &str,
) -> Result<Vec<SearchResult>, String> {
    let endpoint = provider_endpoint("brave", "https://api.search.brave.com/res/v1/web/search");
    let client = crate::coding_tools::http_client::client_following(
        &endpoint,
        crate::coding_tools::http_client::DEFAULT_TIMEOUT,
    )?;
    let url = format!(
        "{}?{}",
        endpoint,
        rightkit_http::form_encode(&[("q", query), ("count", &count.to_string())])
    );

    let request = Request::get(url)
        .secret_header("X-Subscription-Token", api_key)
        .header("Accept", "application/json");
    let resp = crate::coding_tools::http_client::send_any(&client, &request).await?;

    if !crate::coding_tools::http_client::is_success(&resp) {
        return Err(format!("Brave API error {}: {}", resp.status, resp.text()));
    }

    let body: Value = resp.json().map_err(|e| e.to_string())?;
    let mut results = Vec::new();

    if let Some(web_results) = body["web"]["results"].as_array() {
        for item in web_results {
            results.push(SearchResult {
                title: sanitize_search_field(item["title"].as_str().unwrap_or("")),
                snippet: sanitize_search_field(item["description"].as_str().unwrap_or("")),
                url: item["url"].as_str().unwrap_or("").to_string(),
            });
        }
    }

    Ok(results)
}

async fn search_tavily(
    query: &str,
    count: usize,
    api_key: &str,
) -> Result<Vec<SearchResult>, String> {
    let endpoint = provider_endpoint("tavily", "https://api.tavily.com/search");
    let client = crate::coding_tools::http_client::client_following(
        &endpoint,
        crate::coding_tools::http_client::DEFAULT_TIMEOUT,
    )?;
    let body = json!({
        "query": query,
        "max_results": count,
        "api_key": api_key
    });

    let request = Request::post(endpoint)
        .redact_value(api_key)
        .json(&body)
        .map_err(|e| e.to_string())?;
    let resp = crate::coding_tools::http_client::send_any(&client, &request).await?;

    if !crate::coding_tools::http_client::is_success(&resp) {
        return Err(format!("Tavily API error {}: {}", resp.status, resp.text()));
    }

    let resp_body: Value = resp.json().map_err(|e| e.to_string())?;
    let mut results = Vec::new();

    if let Some(items) = resp_body["results"].as_array() {
        for item in items {
            results.push(SearchResult {
                title: sanitize_search_field(item["title"].as_str().unwrap_or("")),
                snippet: sanitize_search_field(item["content"].as_str().unwrap_or("")),
                url: item["url"].as_str().unwrap_or("").to_string(),
            });
        }
    }

    Ok(results)
}

async fn search_exa(query: &str, count: usize, api_key: &str) -> Result<Vec<SearchResult>, String> {
    let endpoint = provider_endpoint("exa", "https://api.exa.ai/search");
    let client = crate::coding_tools::http_client::client_following(
        &endpoint,
        crate::coding_tools::http_client::DEFAULT_TIMEOUT,
    )?;
    let body = json!({
        "query": query,
        "numResults": count,
        "useAutoprompt": true
    });

    let request = Request::post(endpoint)
        .secret_header("x-api-key", api_key)
        .json(&body)
        .map_err(|e| e.to_string())?;
    let resp = crate::coding_tools::http_client::send_any(&client, &request).await?;

    if !crate::coding_tools::http_client::is_success(&resp) {
        return Err(format!("Exa API error {}: {}", resp.status, resp.text()));
    }

    let resp_body: Value = resp.json().map_err(|e| e.to_string())?;
    let mut results = Vec::new();

    if let Some(items) = resp_body["results"].as_array() {
        for item in items {
            results.push(SearchResult {
                title: sanitize_search_field(item["title"].as_str().unwrap_or("")),
                snippet: sanitize_search_field(item["text"].as_str().unwrap_or("")),
                url: item["url"].as_str().unwrap_or("").to_string(),
            });
        }
    }

    Ok(results)
}

fn sanitize_search_field(text: &str) -> String {
    crate::coding_tools::browser_fetch::sanitize_untrusted_tool_text("web_search", text)
}
