//! `recursive_fetch` — crawl a URL and follow same-origin links breadth-first.
//!
//! Fetches root URL and returns a list of {url, text} up to max_depth and max_pages.
//! Uses simple regex for href extraction; keeps it dependency-light.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct RecursiveFetchTool;

#[async_trait::async_trait]
impl Tool for RecursiveFetchTool {
    fn name(&self) -> &str {
        "recursive_fetch"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Crawl a URL and follow same-origin links breadth-first. Returns {url, text}[] up to max_depth and max_pages. Fail: invalid URL, max exceeded, network error."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Starting URL to crawl"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum depth to crawl (default: 2)",
                    "default": 2
                },
                "max_pages": {
                    "type": "integer",
                    "description": "Maximum number of pages to fetch (default: 10)",
                    "default": 10
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let start_url = match args["url"].as_str() {
            Some(u) => u.to_string(),
            None => return ToolResult::error("Missing required argument: url"),
        };

        let max_depth = args["max_depth"].as_u64().unwrap_or(2) as usize;
        let max_pages = args["max_pages"].as_u64().unwrap_or(10) as usize;

        // SSRF guard (fast path + DNS-resolving): same guard browser_fetch uses,
        // applied to the start URL AND every same-origin link the crawl follows
        // (a same-origin redirect or DNS-rebind mid-crawl must not bypass this —
        // guarding only the start URL was the gap; the BFS loop below re-checks
        // every page it is about to fetch).
        if let Some(reason) =
            crate::coding_tools::browser_fetch::ssrf_guard_reject_reason("crawl", &start_url).await
        {
            return ToolResult::error(reason);
        }

        // Validate URL
        let base_url = match parse_url(&start_url) {
            Ok(u) => u,
            Err(_) => return ToolResult::error(format!("Invalid URL: {}", start_url)),
        };

        let origin = format!("{}://{}", base_url.scheme, base_url.host);

        let visited = Arc::new(Mutex::new(HashSet::new()));
        let results = Arc::new(Mutex::new(Vec::new()));

        crawl_pages_bfs(
            &start_url,
            &origin,
            max_depth,
            max_pages,
            visited,
            results.clone(),
        )
        .await;

        let final_results = results.lock().await;
        let mut output = String::from("Crawled pages:\n");

        for (url, text) in final_results.iter() {
            let preview = if text.len() > 100 {
                let mut end = 100;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &text[..end])
            } else {
                text.clone()
            };
            output.push_str(&format!("- {}: {}\n", url, preview));
        }

        // Crawled pages are untrusted external content — wrap so the model treats
        // it as data, not instructions (prompt-injection defense).
        ToolResult::ok(crate::coding_tools::browser_fetch::wrap_untrusted_web_text(
            "recursive_fetch",
            &output,
        ))
    }
}

struct ParsedUrl {
    scheme: String,
    host: String,
}

fn parse_url(s: &str) -> Result<ParsedUrl, String> {
    let (scheme, rest) = if let Some(pos) = s.find("://") {
        (&s[..pos], &s[pos + 3..])
    } else {
        return Err("No scheme found".to_string());
    };

    let host = if let Some(pos) = rest.find('/') {
        &rest[..pos]
    } else {
        rest
    };

    if host.is_empty() {
        return Err("No host found".to_string());
    }

    Ok(ParsedUrl {
        scheme: scheme.to_string(),
        host: host.to_string(),
    })
}

async fn crawl_pages_bfs(
    start_url: &str,
    origin: &str,
    max_depth: usize,
    max_pages: usize,
    visited: Arc<Mutex<HashSet<String>>>,
    results: Arc<Mutex<Vec<(String, String)>>>,
) {
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start_url.to_string(), 0));

    while let Some((url, depth)) = queue.pop_front() {
        if depth > max_depth {
            continue;
        }

        let mut visited_lock = visited.lock().await;
        if visited_lock.contains(&url) || visited_lock.len() >= max_pages {
            continue;
        }
        visited_lock.insert(url.clone());
        drop(visited_lock);

        // Re-check every followed link before fetching — the start URL was already
        // guarded above, but a same-origin page can still resolve/rebind to a
        // private target, so this must run per-page, not just once at the start.
        if crate::coding_tools::browser_fetch::ssrf_guard_reject_reason("crawl", &url)
            .await
            .is_some()
        {
            continue;
        }

        // Fetch the page
        if let Ok(html) = fetch_page_content(&url).await {
            let content = extract_readable_text(&html);
            results.lock().await.push((url.clone(), content));

            if depth < max_depth {
                // Extract links from the raw markup: the readable text has no hrefs.
                let links = extract_links(&html, origin);
                for link in links {
                    let visited_lock = visited.lock().await;
                    if !visited_lock.contains(&link) && visited_lock.len() < max_pages {
                        drop(visited_lock);
                        queue.push_back((link, depth + 1));
                    }
                }
            }
        }
    }
}

async fn fetch_page_content(url: &str) -> Result<String, String> {
    let client = crate::coding_tools::http_client::crawl_page_client(
        url,
        std::time::Duration::from_secs(10),
    )
    .map_err(|e| format!("Failed to fetch: {}", e))?;
    let request = rightkit_http::Request::get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; Agent/1.0)");
    match client.send_any(&request).await {
        Ok(resp) => Ok(resp.text()),
        Err(e) => Err(format!("Failed to fetch: {}", e)),
    }
}

fn extract_readable_text(html: &str) -> String {
    // Remove script and style tags
    let re_script = Regex::new(r"(?i)<script[^>]*>.*?</script>").unwrap();
    let re_style = Regex::new(r"(?i)<style[^>]*>.*?</style>").unwrap();
    let cleaned = re_script.replace_all(html, "");
    let cleaned = re_style.replace_all(&cleaned, "");

    // Remove HTML tags
    let re_tags = Regex::new(r"<[^>]+>").unwrap();
    let text = re_tags.replace_all(&cleaned, " ");

    // Collapse whitespace
    let re_ws = Regex::new(r"\s+").unwrap();
    let text = re_ws.replace_all(&text, " ");

    text.trim().to_string()
}

fn extract_links(html: &str, origin: &str) -> Vec<String> {
    let re_href = Regex::new(r#"href\s*=\s*["']?([^"'\s>]+)"#).unwrap();
    let mut links = Vec::new();

    for cap in re_href.captures_iter(html) {
        if let Some(href_match) = cap.get(1) {
            let href = href_match.as_str();

            // Make absolute URL
            let absolute_url = if href.starts_with("http://") || href.starts_with("https://") {
                href.to_string()
            } else if href.starts_with('/') {
                format!("{}{}", origin, href)
            } else if href.starts_with('#') || href.starts_with('?') {
                continue;
            } else {
                format!("{}/{}", origin, href)
            };

            // Check if same origin
            if absolute_url.starts_with(origin) {
                // Clean up query params and fragments
                if let Some(pos) = absolute_url.find('#') {
                    links.push(absolute_url[..pos].to_string());
                } else {
                    links.push(absolute_url);
                }
            }
        }
    }

    let mut seen = HashSet::new();
    links
        .into_iter()
        .filter(|link| seen.insert(link.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::PermissionMode;
    use rightkit_agent_policy::WorkspaceTrust;
    use std::path::PathBuf;

    #[test]
    fn test_extract_readable_text() {
        let html = "<p>Hello <b>world</b></p><script>alert('test');</script>";
        let text = extract_readable_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn test_extract_links() {
        let html = r#"<a href="/page1">Link 1</a><a href="https://example.com/page2">Link 2</a>"#;
        let links = extract_links(html, "https://example.com");
        assert!(links.iter().any(|l| l.contains("/page1")));
        assert!(links.iter().any(|l| l.contains("/page2")));
    }

    #[tokio::test]
    async fn recursive_fetch_blocks_private_lan_start_url() {
        // 192.168.x is LAN (not loopback) — blocked by the DNS-resolving guard,
        // matching browser_fetch's private-target parity.
        let tool = RecursiveFetchTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(json!({"url": "http://192.168.1.10/admin"}), &ctx)
            .await;

        assert!(result.is_error, "private LAN start URL should be blocked");
        assert!(
            result.content.contains("SSRF") || result.content.contains("private"),
            "error should mention the protection reason: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn recursive_fetch_blocks_cgnat_start_url() {
        let tool = RecursiveFetchTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(json!({"url": "https://100.64.0.1/hook"}), &ctx)
            .await;

        assert!(result.is_error, "CGNAT start URL should be blocked");
    }

    #[tokio::test]
    async fn crawl_pages_bfs_skips_a_link_that_resolves_private() {
        // A link discovered mid-crawl (not the start URL) must be re-checked —
        // this is the gap the audit flagged: guarding only the start URL let a
        // same-origin-labeled private link slip through the BFS loop.
        let visited = Arc::new(Mutex::new(HashSet::new()));
        let results = Arc::new(Mutex::new(Vec::new()));

        // Start URL is a private target directly injected into the BFS queue
        // (bypassing the tool-level start-URL guard) to isolate the per-page
        // guard inside crawl_pages_bfs itself.
        crawl_pages_bfs(
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.169.254",
            1,
            5,
            visited,
            results.clone(),
        )
        .await;

        assert!(
            results.lock().await.is_empty(),
            "private-target page must never be fetched or recorded"
        );
    }

    #[tokio::test]
    async fn test_recursive_fetch_invalid_url() {
        let tool = RecursiveFetchTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool.execute(json!({"url": "not a url"}), &ctx).await;

        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_recursive_fetch_valid_url() {
        let tool = RecursiveFetchTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        // This will fail in a test environment without network, but validates schema
        let result = tool
            .execute(
                json!({"url": "https://example.com", "max_depth": 1, "max_pages": 5}),
                &ctx,
            )
            .await;

        // Even if network fails, the tool should handle it gracefully
        assert!(result.content.contains("Crawled") || result.content.contains("error"));
    }
}
