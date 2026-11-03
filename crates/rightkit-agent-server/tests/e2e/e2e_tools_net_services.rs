//! Network-facing tool journeys through the REAL `coderight run` binary, against loopback fakes
//! started by the test (no internet): browser_fetch, recursive_fetch, web_search, context7, the
//! tunnel pair's guards, image_generate's guards, and the typed "not configured" contract of the
//! github tool in a headless run (flyio/cloudflare use the interactive credential flow, which is
//! covered by the TUI credential journeys).
//! The fakes record every request, so the tests assert both what the model was shown and what the
//! tool actually put on the wire.
#![allow(dead_code, unused_imports)]

use crate::support::e2e_tools as t;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use t::*;

// ───────────────────────────── loopback fake server ───────────────────────────

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Resp {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    headers: Vec<(&'static str, String)>,
}

impl Resp {
    fn html(body: &str) -> Self {
        Resp {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
            headers: vec![],
        }
    }
    fn json(body: Value) -> Self {
        Resp {
            status: 200,
            content_type: "application/json",
            body: body.to_string().into_bytes(),
            headers: vec![],
        }
    }
    fn status(code: u16, body: &str) -> Self {
        Resp {
            status: code,
            content_type: "text/plain",
            body: body.as_bytes().to_vec(),
            headers: vec![],
        }
    }
    fn redirect(to: &str) -> Self {
        Resp {
            status: 302,
            content_type: "text/plain",
            body: Vec::new(),
            headers: vec![("Location", to.to_string())],
        }
    }
}

struct FakeHttp {
    port: u16,
    seen: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn read_http_request(stream: &mut TcpStream) -> Option<Req> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .ok();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.lines();
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut body).ok()?;
    }
    Some(Req {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

impl FakeHttp {
    fn start(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> Self {
        let handler = Arc::new(handler);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake http");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Req>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen2, stop2) = (seen.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                };
                let (handler, seen) = (handler.clone(), seen2.clone());
                std::thread::spawn(move || {
                    stream.set_nonblocking(false).ok();
                    let Some(req) = read_http_request(&mut stream) else {
                        return;
                    };
                    seen.lock().unwrap().push(req.clone());
                    let resp = handler(&req);
                    let mut head = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        resp.status,
                        resp.content_type,
                        resp.body.len()
                    );
                    for (k, v) in &resp.headers {
                        head.push_str(&format!("{k}: {v}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&resp.body);
                    let _ = stream.flush();
                });
            }
        });
        FakeHttp {
            port,
            seen,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    fn requests(&self) -> Vec<Req> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Run the rig with extra environment variables (the tools read their overrides in-process).
fn run_with_env(rig: &Rig, env: &[(&str, String)], prompt: &str) -> Out {
    let mut c = rig.command(&["--stream-json", "--bypass-approvals"]);
    // No ambient search-provider credentials may leak into a hermetic journey.
    for k in [
        "TINYFISH_API_KEY",
        "BRAVE_API_KEY",
        "TAVILY_API_KEY",
        "EXA_API_KEY",
    ] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.arg(prompt);
    t::cli::finish(c, 120)
}

// ──────────────────────────────── browser_fetch ───────────────────────────────

const FETCH_PAGE: &str = "<!doctype html><html><head><title>Fetch Title</title><style>.x{color:red}</style></head><body><h1>Heading-Fetch</h1><p>fetch-body-text</p><script>var leaked = 'script-source-should-not-appear';</script></body></html>";

#[test]
fn browser_fetch_extracts_readable_text_follows_redirects_and_enforces_the_guards() {
    let site = FakeHttp::start(|req| match req.path.as_str() {
        "/" => Resp::html(FETCH_PAGE),
        "/redirect" => Resp::redirect("/"),
        "/blocked" => Resp::status(403, "no bots"),
        "/missing" => Resp::status(404, "nope"),
        "/binary" => Resp {
            status: 200,
            content_type: "application/octet-stream",
            body: vec![0, 159, 146, 150, 0, 1, 2, 3],
            headers: vec![],
        },
        _ => Resp::status(404, "nope"),
    });
    let p = sequence(
        vec![
            call(
                "browser_fetch",
                json!({"url": site.url("/"), "tier":"native"}),
            ), // 0
            call("browser_fetch", json!({"url": site.url("/redirect")})), // 1 (auto)
            call(
                "browser_fetch",
                json!({"url": site.url("/blocked"), "tier":"native"}),
            ), // 2
            call("browser_fetch", json!({"url": site.url("/missing")})),  // 3
            call(
                "browser_fetch",
                json!({"url": site.url("/binary"), "tier":"native"}),
            ), // 4
            call(
                "browser_fetch",
                json!({"url":"http://169.254.169.254/latest/meta-data/"}),
            ), // 5
            call("browser_fetch", json!({"url":"file:///C:/Windows/win.ini"})), // 6
            call(
                "browser_fetch",
                json!({"url":"http://user:pw@127.0.0.1:1/x"}),
            ), // 7
            call("browser_fetch", json!({})),                             // 8
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 9);

    // 0: readable text only, wrapped as untrusted web content.
    assert!(!r[0].error, "{}", r[0].text);
    assert!(r[0].text.contains("Fetch Title"), "{}", r[0].text);
    assert!(r[0].text.contains("Heading-Fetch"), "{}", r[0].text);
    assert!(r[0].text.contains("fetch-body-text"), "{}", r[0].text);
    assert!(
        !r[0].text.contains("script-source-should-not-appear"),
        "script bodies must be stripped: {}",
        r[0].text
    );
    assert!(
        r[0].text.contains("coderight-untrusted-context"),
        "fetched text must be fenced as untrusted: {}",
        r[0].text
    );
    // 1: auto tier follows the redirect to the real page.
    assert!(!r[1].error, "{}", r[1].text);
    assert!(r[1].text.contains("fetch-body-text"), "{}", r[1].text);
    // 2: a 403 is retried once with browser-like headers, then reported as blocked.
    assert!(r[2].error);
    assert!(
        r[2].text.contains("blocked even with stealth headers"),
        "{}",
        r[2].text
    );
    let blocked: Vec<Req> = site
        .requests()
        .into_iter()
        .filter(|q| q.path == "/blocked")
        .collect();
    assert_eq!(
        blocked.len(),
        2,
        "plain attempt then stealth retry: {blocked:?}"
    );
    assert!(
        blocked[1]
            .header("user-agent")
            .unwrap_or_default()
            .starts_with("Mozilla/5.0 (Windows NT"),
        "retry must carry browser headers: {blocked:?}"
    );
    // 3-4: HTTP errors and non-text payloads are errors, not content.
    assert!(r[3].error);
    assert!(r[3].text.contains("HTTP 404"), "{}", r[3].text);
    assert!(r[4].error, "{}", r[4].text);
    // 5-8: SSRF / scheme / credential / argument guards, none of which reached the fake.
    assert!(r[5].error, "{}", r[5].text);
    assert!(r[6].error);
    assert!(
        r[6].text.contains("unsupported URL scheme 'file'"),
        "{}",
        r[6].text
    );
    assert!(r[7].error);
    assert!(
        r[7].text.contains("URL credentials are not allowed"),
        "{}",
        r[7].text
    );
    assert!(r[8].error);
    // EFF-001: engine schema admission rejects malformed calls before dispatch
    // (engine/src/tool_dispatch_plumbing.rs), with a repairable schema diagnostic.
    assert!(
        r[8].text
            .starts_with("Invalid tool arguments: $.url is required."),
        "{}",
        r[8].text
    );
    assert!(
        r[8].text.contains("Expected input schema:"),
        "{}",
        r[8].text
    );
}

// ─────────────────────────────── recursive_fetch ──────────────────────────────

#[test]
fn recursive_fetch_crawls_same_origin_links_breadth_first_within_its_bounds() {
    let site = FakeHttp::start(|req| {
        match req.path.as_str() {
        "/" => Resp::html(
            "<html><body>ROOT-PAGE <a href=\"/a\">a</a> <a href=\"/b\">b</a> <a href=\"https://elsewhere.invalid/off\">off</a></body></html>",
        ),
        "/a" => Resp::html("<html><body>PAGE-A <a href=\"/c\">c</a></body></html>"),
        "/b" => Resp::html("<html><body>PAGE-B</body></html>"),
        "/c" => Resp::html("<html><body>PAGE-C</body></html>"),
        _ => Resp::status(404, "nope"),
    }
    });
    let p = sequence(
        vec![
            call(
                "recursive_fetch",
                json!({"url": site.url("/"), "max_depth": 1, "max_pages": 10}),
            ), // 0
            call(
                "recursive_fetch",
                json!({"url": site.url("/"), "max_depth": 2, "max_pages": 10}),
            ), // 1
            call(
                "recursive_fetch",
                json!({"url": site.url("/"), "max_depth": 2, "max_pages": 2}),
            ), // 2
            call("recursive_fetch", json!({"url": "http://169.254.169.254/"})), // 3
            call("recursive_fetch", json!({})),                                 // 4
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 5);

    let page_lines = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with("- http"))
            .map(str::to_string)
            .collect()
    };

    // depth 1: root + its two same-origin links, in breadth-first order; nothing off-origin.
    let d1 = page_lines(&r[0].text);
    assert_eq!(d1.len(), 3, "{}", r[0].text);
    assert!(d1[0].contains("ROOT-PAGE"), "{d1:?}");
    assert!(d1[1].contains("PAGE-A"), "{d1:?}");
    assert!(d1[2].contains("PAGE-B"), "{d1:?}");
    assert!(!r[0].text.contains("PAGE-C"), "{}", r[0].text);
    assert!(
        !site.requests().iter().any(|q| q.path.contains("off")),
        "off-origin links must never be fetched"
    );
    assert!(r[0].text.contains("coderight-untrusted-context"));

    // depth 2 reaches /c through /a.
    let d2 = page_lines(&r[1].text);
    assert_eq!(d2.len(), 4, "{}", r[1].text);
    assert!(r[1].text.contains("PAGE-C"), "{}", r[1].text);

    // max_pages bounds the crawl.
    assert_eq!(page_lines(&r[2].text).len(), 2, "{}", r[2].text);

    assert!(r[3].error, "{}", r[3].text);
    assert!(r[4].error);
    assert!(
        r[4].text
            .starts_with("Invalid tool arguments: $.url is required."),
        "{}",
        r[4].text
    );
    assert!(
        r[4].text.contains("Expected input schema:"),
        "{}",
        r[4].text
    );
}

// ───────────────────────────────── web_search ─────────────────────────────────

const DDG_PAGE: &str = r#"<html><body>
<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.invalid%2Fone&rut=x">First Title</a><a class="result__snippet">First snippet text</a></div>
<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.invalid%2Ftwo&rut=y">Second Title</a><a class="result__snippet">Second snippet text</a></div>
<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.invalid%2Fthree&rut=z">Third Title</a><a class="result__snippet">Third snippet text</a></div>
</body></html>"#;

#[test]
fn web_search_queries_the_provider_and_returns_parsed_results() {
    let engine = FakeHttp::start(|req| {
        if req.method == "POST" && req.path == "/html/" {
            if req.body.contains("q=empty-results") {
                Resp::html("<html><body>nothing</body></html>")
            } else if req.body.contains("q=server-down") {
                Resp::status(503, "down")
            } else {
                Resp::html(DDG_PAGE)
            }
        } else {
            Resp::status(404, "nope")
        }
    });
    let p = sequence(
        vec![
            call(
                "web_search",
                json!({"query":"rust async traits", "count": 2}),
            ), // 0
            call("web_search", json!({"query":"empty-results"})), // 1
            call("web_search", json!({"query":"server-down"})),   // 2
            call("web_search", json!({})),                        // 3
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = run_with_env(
        &rig,
        &[("RIGHTKIT_WEB_SEARCH_DUCKDUCKGO_URL", engine.url("/html/"))],
        "go",
    );
    let r = expect_results(&out, 4);

    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text.contains("coderight-untrusted-context"),
        "{}",
        r[0].text
    );
    assert!(r[0].text.contains("First Title"), "{}", r[0].text);
    assert!(
        r[0].text.contains("https://example.invalid/one"),
        "{}",
        r[0].text
    );
    assert!(r[0].text.contains("Second snippet text"), "{}", r[0].text);
    assert!(
        !r[0].text.contains("Third Title"),
        "count=2 must bound the results: {}",
        r[0].text
    );
    let sent = engine.requests();
    let first = sent.first().expect("the provider was queried");
    assert_eq!(first.method, "POST");
    assert!(
        first.body.contains("q=rust+async+traits")
            || first.body.contains("q=rust%20async%20traits"),
        "query must reach the provider: {}",
        first.body
    );

    assert!(r[1].error);
    assert!(
        r[1].text
            .contains("DuckDuckGo returned no parseable search results"),
        "{}",
        r[1].text
    );
    assert!(r[2].error);
    assert!(r[2].text.contains("HTTP 503"), "{}", r[2].text);
    assert!(r[3].error);
    assert!(
        r[3].text
            .starts_with("Invalid tool arguments: $.query is required"),
        "{}",
        r[3].text
    );
}

// ───────────────────────────────── context7 ───────────────────────────────────

#[test]
fn context7_resolves_library_ids_and_fetches_topic_docs() {
    let api = FakeHttp::start(|req| {
        if req.path.starts_with("/search?q=react") {
            Resp::json(
                json!({"results":[{"id":"/facebook/react","title":"React","description":"UI library"}]}),
            )
        } else if req.path.starts_with("/search?") {
            Resp::json(json!({"results":[]}))
        } else if req.path.starts_with("/libraries/%2Ffacebook%2Freact") {
            Resp::json(json!({"content":"DOCS-ABOUT-HOOKS: use effects carefully"}))
        } else {
            Resp::status(404, "nope")
        }
    });
    let p = sequence(
        vec![
            call(
                "context7",
                json!({"operation":"resolve-library-id","libraryName":"react"}),
            ), // 0
            call(
                "context7",
                json!({"operation":"resolve-library-id","libraryName":"unknown-lib"}),
            ), // 1
            call(
                "context7",
                json!({"operation":"get-library-docs","context7CompatibleLibraryID":"/facebook/react","topic":"hooks","tokens":2000}),
            ), // 2
            call(
                "context7",
                json!({"operation":"get-library-docs","context7CompatibleLibraryID":"/nobody/nothing"}),
            ), // 3
            call("context7", json!({"operation":"resolve-library-id"})), // 4
            call("context7", json!({"operation":"get-library-docs"})),   // 5
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = run_with_env(&rig, &[("RIGHTKIT_CONTEXT7_API_BASE", api.url(""))], "go");
    let r = expect_results(&out, 6);

    assert!(!r[0].error, "{}", r[0].text);
    let found: Value = serde_json::from_str(&r[0].text).expect("results JSON");
    assert_eq!(found[0]["id"], "/facebook/react");
    assert_eq!(
        serde_json::from_str::<Value>(&r[1].text).unwrap(),
        json!([])
    );
    assert_eq!(r[2].text, "DOCS-ABOUT-HOOKS: use effects carefully");
    let docs_request = api
        .requests()
        .into_iter()
        .find(|q| q.path.starts_with("/libraries/"))
        .expect("docs were requested");
    assert!(
        docs_request.path.contains("topic=hooks"),
        "{}",
        docs_request.path
    );
    assert!(
        docs_request.path.contains("tokens=2000"),
        "{}",
        docs_request.path
    );
    assert!(r[3].error);
    assert!(
        r[3].text
            .starts_with("context7 docs fetch failed: HTTP 404"),
        "{}",
        r[3].text
    );
    assert!(r[4].error);
    assert_eq!(
        r[4].text,
        "'libraryName' is required for resolve-library-id"
    );
    assert!(r[5].error);
    assert_eq!(
        r[5].text,
        "'context7CompatibleLibraryID' is required for get-library-docs"
    );
}

// ──────────────────────────── tunnel / dev_tunnel_proxy ───────────────────────

#[test]
fn tunnel_tools_list_providers_and_enforce_every_local_guard_before_any_provider_call() {
    // A live loopback listener the guards can probe.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let live_port = listener.local_addr().unwrap().port();
    let p = sequence(
        vec![
            call("tunnel", json!({"action":"list_providers"})), // 0
            call(
                "tunnel",
                json!({"action":"start","provider":"cloudflare_tunnel","local_port":live_port}),
            ), // 1 no configure
            call(
                "tunnel",
                json!({"action":"configure","provider":"cloudflare_tunnel","token":"test-only-secret-value","account_id":"acct"}),
            ), // 2
            call(
                "tunnel",
                json!({"action":"start","provider":"cloudflare_tunnel","local_host":"10.0.0.5","local_port":live_port}),
            ), // 3 non-loopback
            call(
                "tunnel",
                json!({"action":"start","provider":"cloudflare_tunnel","local_port":1}),
            ), // 4 nothing listening
            call(
                "tunnel",
                json!({"action":"start","provider":"cloudflare_tunnel","local_port":live_port,"keep_alive":true}),
            ), // 5 keep_alive refused
            call(
                "tunnel",
                json!({"action":"start","provider":"no-such-provider","local_port":live_port}),
            ), // 6
            call(
                "dev_tunnel_proxy",
                json!({"action":"start","provider":"cloudflare_tunnel","local_port":1}),
            ), // 7 shares the configured credential, nothing listening
            call(
                "dev_tunnel_proxy",
                json!({"action":"start","provider":"no-such-provider","local_port":live_port}),
            ), // 8
            call("dev_tunnel_proxy", json!({"action":"start"})), // 9
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 10);

    let providers: Value = serde_json::from_str(&r[0].text).expect("providers JSON");
    assert!(
        providers["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "cloudflare_tunnel"),
        "{providers}"
    );
    assert!(r[1].error);
    assert_eq!(
        r[1].text,
        "No managed token configured for cloudflare_tunnel in this session; run action=configure first."
    );
    assert_eq!(r[2].text, "Configured managed token for cloudflare_tunnel.");
    for result in &r {
        assert!(
            !result.text.contains("test-only-secret-value"),
            "the provider credential must never be echoed to the model: {}",
            result.text
        );
    }
    assert!(r[3].error);
    assert_eq!(
        r[3].text,
        "Only a CodeRight loopback endpoint (127.0.0.1, ::1, or localhost) may be exposed."
    );
    assert!(r[4].error);
    assert_eq!(
        r[4].text,
        "No CodeRight endpoint is listening at 127.0.0.1:1."
    );
    assert!(r[5].error);
    assert!(
        r[5].text
            .starts_with("cloudflare_tunnel exposures are session-bound"),
        "{}",
        r[5].text
    );
    assert!(r[6].error);
    assert_eq!(r[6].text, "Unknown tunnel provider: no-such-provider");
    assert!(r[7].error);
    assert!(
        r[7].text
            .contains("No CodeRight endpoint is listening at 127.0.0.1:1."),
        "{}",
        r[7].text
    );
    assert!(r[8].error);
    assert_eq!(r[8].text, "Unknown tunnel provider: no-such-provider");
    assert!(r[9].error);
    assert!(
        r[9].text
            .starts_with("Invalid tool arguments: $.provider is required"),
        "{}",
        r[9].text
    );
    drop(listener);
}

// ───────────────────────────── image_generate guards ──────────────────────────

#[test]
fn image_generate_validates_requests_and_reports_an_unconfigured_account() {
    let p = sequence(
        vec![
            call(
                "image_generate",
                json!({"model":"no-such-image-model","prompt":"a cat"}),
            ), // 0
            call("image_generate", json!({"prompt":"a cat"})), // 1
            call("image_generate", json!({"model":"gpt-image-1"})), // 2
            call(
                "image_generate",
                json!({"model":"gpt-image-1","prompt":"a cat","size":"huge"}),
            ), // 3
            call(
                "image_generate",
                json!({"model":"gpt-image-1","prompt":"a cat","count":9}),
            ), // 4
            call(
                "image_generate",
                json!({"model":"gpt-image-1","prompt":"a cat","format":"gif"}),
            ), // 5
            call(
                "image_generate",
                json!({"model":"gpt-image-1","prompt":"a cat"}),
            ), // 6
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let mut c = rig.command(&["--stream-json", "--bypass-approvals"]);
    // No image-provider account may exist for this journey.
    for k in ["OPENAI_API_KEY", "RIGHTKIT_OPENAI_API_KEY"] {
        c.env_remove(k);
    }
    c.arg("go");
    let out = t::cli::finish(c, 120);
    let r = expect_results(&out, 7);

    assert!(r[0].error);
    assert_eq!(r[0].text, "Unknown image model: no-such-image-model");
    assert!(r[1].error);
    assert!(
        r[1].text
            .starts_with("Invalid tool arguments: $.model is required"),
        "{}",
        r[1].text
    );
    assert!(r[2].error);
    assert!(
        r[2].text
            .starts_with("Invalid tool arguments: $.prompt is required"),
        "{}",
        r[2].text
    );
    assert!(r[3].error);
    // crates/tools/src/image_generate.rs:121-125: the model's size allowlist rejects "huge"
    // before any format check; no WIDTHxHEIGHT check exists in CodeRight or rightkit.
    assert_eq!(
        r[3].text,
        "image model 'gpt-image-1' does not support size huge"
    );
    assert!(r[4].error);
    assert_eq!(r[4].text, "count must be between 1 and 4");
    // The `format` argument is gone: the published rightkit-llm `ImageRequest` (0.3.0,
    // generation.rs:33-38) has no format field, and d39a576 dropped it from the schema.
    // An extra `format` argument is ignored, so the call reaches the account check.
    assert!(r[5].error);
    assert!(
        r[5].text
            .ends_with("image account is not configured on this execution target."),
        "{}",
        r[5].text
    );
    assert!(r[6].error);
    assert!(
        r[6].text
            .ends_with("image account is not configured on this execution target."),
        "{}",
        r[6].text
    );
}

// ─────────────── credentialed provider tools in a headless run ────────────────

#[test]
fn github_reports_a_typed_not_configured_error_without_credentials() {
    let p = sequence(
        vec![call(
            "github",
            json!({"action":"list_prs","owner":"octo","repo":"hello"}),
        )],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 1);
    assert!(r[0].error);
    assert_eq!(
        r[0].text,
        "No GitHub credentials found. Sign in with GitHub in CodeRight, run `gh auth login`, or set GH_TOKEN."
    );
}
