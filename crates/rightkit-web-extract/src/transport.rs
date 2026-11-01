//! A ready-made [`ArticleNet`] over `rightkit-http`: the strict public-web address policy, one
//! GET per call with no redirect followed (the fetch tiers walk and vet every hop themselves),
//! and a header-only probe.
//!
//! Hosts with their own network stack implement [`ArticleNet`] and [`PageClient`] instead.

use std::time::Duration;

use rightkit_http::urlguard;
use rightkit_http::{Client, Request};

use crate::net::{ArticleNet, PageClient, PageResponse};

/// The strict public-web policy of `rightkit-http`, with environment proxies honoured.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpNet;

struct HttpClient {
    client: Client,
    timeout: Duration,
}

impl HttpClient {
    fn request(&self, url: &str, headers: &[(&str, &str)]) -> Request {
        let mut request = Request::get(url).timeout(self.timeout);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request
    }
}

impl PageClient for HttpClient {
    fn probe(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, Option<String>), String> {
        let head = self
            .client
            .send_headers(&self.request(url, headers))
            .map_err(|e| urlguard::error_text(&e))?;
        let location = head.header("location").map(str::to_string);
        Ok((head.status, location))
    }

    fn stream(&self, url: &str, headers: &[(&str, &str)]) -> Result<PageResponse, String> {
        let response = self
            .client
            .stream_any(&self.request(url, headers))
            .map_err(|e| urlguard::error_text(&e))?;
        let location = response.header("location").map(str::to_string);
        let content_type = response.header("content-type").map(str::to_string);
        Ok(PageResponse {
            status: response.status,
            location,
            content_type,
            body: response.into_reader(),
        })
    }
}

impl ArticleNet for HttpNet {
    fn ensure_public_http_url(&self, url: &str) -> Result<(), String> {
        urlguard::ensure_public_http_url(url)
    }

    fn vet_resolved_host(&self, url: &str) -> Result<(), String> {
        urlguard::vet_resolved_host(url)
    }

    fn client(&self, user_agent: &str, timeout: Duration) -> Box<dyn PageClient + '_> {
        Box::new(HttpClient {
            client: urlguard::public_web_client(user_agent, timeout, 0),
            timeout,
        })
    }
}

/// Whether a failure text is an address-policy refusal (final, never retried elsewhere).
pub fn is_policy_refusal(message: &str) -> bool {
    urlguard::is_policy_refusal(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_urls_are_refused_before_any_connection() {
        let net = HttpNet;
        assert!(net.ensure_public_http_url("http://127.0.0.1/").is_err());
        assert!(net.ensure_public_http_url("ftp://example.com/").is_err());
        assert!(net.ensure_public_http_url("https://example.com/a").is_ok());
    }
}
