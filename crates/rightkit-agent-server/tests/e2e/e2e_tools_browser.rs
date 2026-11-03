//! Browser tool journeys through the REAL `coderight run` binary against a real headless
//! Chrome/Edge and a tiny HTTP page served from loopback inside the test (no internet).
//! Covers browser_open / browser_read / browser_act / browser_screenshot / browser_console /
//! browser_close and the navigation guard that refuses private and non-http targets.

use crate::support::e2e_tools as t;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use t::*;

const PAGE_ONE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>E2E Browser Page</title></head>
<body>
<h1 id="hello">Hello-E2E-Page</h1>
<input id="name" type="text" placeholder="name">
<select id="color"><option value="red">Red</option><option value="green">Green</option></select>
<button id="go">Go</button>
<a id="next" href="/page2">Next page</a>
<script>
console.log('page-loaded-e2e');
document.getElementById('go').addEventListener('click', function () {
  console.error('boom-e2e');
  var r = document.createElement('div');
  r.id = 'result';
  r.textContent = 'clicked:' + document.getElementById('name').value + ':' + document.getElementById('color').value;
  document.body.appendChild(r);
});
setTimeout(function () { throw new Error('uncaught-e2e'); }, 50);
</script>
</body></html>"#;

const PAGE_TWO: &str =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Second Page</title></head><body><p id=\"p2\">Second-Page-E2E</p></body></html>";

/// Serves the two fixture pages on an ephemeral loopback port until dropped.
struct PageServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PageServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind page server");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                };
                std::thread::spawn(move || {
                    stream.set_nonblocking(false).ok();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .ok();
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let (status, body) = match path.as_str() {
                        "/" => ("200 OK", PAGE_ONE),
                        "/page2" => ("200 OK", PAGE_TWO),
                        _ => ("404 Not Found", "not found"),
                    };
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.flush();
                });
            }
        });
        PageServer {
            port,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for PageServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn browser_installed() -> bool {
    if cfg!(windows) {
        [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
    } else if cfg!(target_os = "macos") {
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
    } else {
        [
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
    }
}

/// `session: <id>` line of a browser_open result.
fn session_of(open_text: &str) -> String {
    open_text
        .lines()
        .find_map(|l| l.strip_prefix("session: "))
        .unwrap_or_else(|| panic!("no session line in {open_text}"))
        .trim()
        .to_string()
}

fn act(action: &'static str, extra: Value) -> Step {
    call_with("browser_act", move |seen| {
        let mut args = json!({"session": session_of(&seen[0]), "action": action});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            args[k] = v;
        }
        args
    })
}

fn on_session(tool: &'static str, extra: Value) -> Step {
    call_with(tool, move |seen| {
        let mut args = json!({"session": session_of(&seen[0])});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            args[k] = v;
        }
        args
    })
}

#[test]
fn browser_tools_drive_a_real_headless_browser_end_to_end() {
    assert!(
        browser_installed(),
        "this journey needs Chrome or Edge on the host (msedge ships with Windows)"
    );
    let server = PageServer::start();
    let page_one = server.url("/");
    let page_two = server.url("/page2");

    let p = sequence(
        vec![
            call("browser_open", json!({ "url": page_one })), // 0
            on_session("browser_read", json!({})),            // 1
            act("type", json!({"selector":"#name","text":"Ada"})), // 2
            act("select", json!({"selector":"#color","text":"green"})), // 3
            act("click", json!({"selector":"#go"})),          // 4
            act("wait_for", json!({"selector":"#result","timeout_ms":10000})), // 5
            on_session("browser_read", json!({})),            // 6
            on_session("browser_console", json!({})),         // 7
            on_session("browser_console", json!({"errors_only":true})), // 8
            on_session("browser_screenshot", json!({})),      // 9
            act("navigate", json!({ "url": page_two })),      // 10
            on_session("browser_read", json!({})),            // 11
            act("back", json!({})),                           // 12
            act("click", json!({"selector":"#nope"})),        // 13
            on_session("browser_close", json!({})),           // 14
            on_session("browser_read", json!({})),            // 15
            on_session("browser_close", json!({})),           // 16
            call(
                "browser_open",
                json!({"url":"http://169.254.169.254/latest/meta-data/"}),
            ), // 17
            call("browser_open", json!({"url":"file:///C:/Windows/win.ini"})), // 18
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 19);

    // 0: opened, real title and url reported.
    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text.starts_with("Opened.\nsession: bs-"),
        "{}",
        r[0].text
    );
    assert!(
        r[0].text.contains("title: E2E Browser Page"),
        "{}",
        r[0].text
    );
    assert!(
        r[0].text.contains(&format!("url: {page_one}")),
        "{}",
        r[0].text
    );

    // 1: rendered text AND the selectors needed to act, fenced as untrusted web content.
    assert!(!r[1].error, "{}", r[1].text);
    assert!(r[1].text.contains("Hello-E2E-Page"), "{}", r[1].text);
    for selector in ["#go", "#name", "#color"] {
        assert!(
            r[1].text.contains(selector),
            "interactive element {selector} missing: {}",
            r[1].text
        );
    }
    assert!(
        r[1].text.to_lowercase().contains("untrusted"),
        "page text must be fenced as untrusted: {}",
        &r[1].text[..r[1].text.len().min(400)]
    );

    // 2-5: real input effects on the page.
    assert!(!r[2].error, "{}", r[2].text);
    assert!(
        r[2].text.starts_with("Typed into '#name'."),
        "{}",
        r[2].text
    );
    assert!(!r[3].error, "{}", r[3].text);
    assert!(
        r[3].text.starts_with("Selected 'green' in '#color'."),
        "{}",
        r[3].text
    );
    assert!(!r[4].error, "{}", r[4].text);
    assert!(r[4].text.starts_with("Clicked '#go'."), "{}", r[4].text);
    assert!(!r[5].error, "{}", r[5].text);
    assert!(
        r[5].text.starts_with("'#result' appeared."),
        "{}",
        r[5].text
    );

    // 6: the click's DOM change is visible to the model on the next read.
    assert!(
        r[6].text.contains("clicked:Ada:green"),
        "the typed value, chosen option and click must all have reached the page: {}",
        r[6].text
    );

    // 7-8: console capture (log + error + uncaught exception), then the errors-only filter.
    assert!(r[7].text.contains("[log] page-loaded-e2e"), "{}", r[7].text);
    assert!(r[7].text.contains("[error] boom-e2e"), "{}", r[7].text);
    assert!(r[7].text.contains("uncaught-e2e"), "{}", r[7].text);
    assert!(r[8].text.contains("[error] boom-e2e"), "{}", r[8].text);
    assert!(
        !r[8].text.contains("page-loaded-e2e"),
        "errors_only must drop plain logs: {}",
        r[8].text
    );

    // 9: a real PNG came back through the image-block contract.
    assert!(!r[9].error, "{}", r[9].text);
    assert!(
        r[9].text.contains("[image-block]"),
        "{}",
        &r[9].text[..r[9].text.len().min(300)]
    );
    assert!(
        r[9].text.contains("mime: image/png"),
        "{}",
        &r[9].text[..r[9].text.len().min(300)]
    );

    // 10-12: navigation and history.
    assert!(!r[10].error, "{}", r[10].text);
    assert!(
        r[10]
            .text
            .starts_with(&format!("Navigated to '{page_two}'.")),
        "{}",
        r[10].text
    );
    assert!(r[11].text.contains("Second-Page-E2E"), "{}", r[11].text);
    assert!(!r[12].error, "{}", r[12].text);
    assert!(r[12].text.starts_with("Went back."), "{}", r[12].text);
    assert!(r[12].text.contains("E2E Browser Page"), "{}", r[12].text);

    // 13: acting on a missing element fails loudly.
    assert!(r[13].error, "{}", r[13].text);

    // 14-16: close, and use-after-close is a typed error.
    let id = session_of(&r[0].text);
    assert_eq!(r[14].text, format!("Closed browser session '{id}'."));
    assert!(r[15].error);
    assert!(
        r[15]
            .text
            .starts_with(&format!("No open browser session '{id}'.")),
        "{}",
        r[15].text
    );
    assert!(r[16].error);
    assert_eq!(r[16].text, format!("No open browser session '{id}'."));

    // 17-18: the navigation guard refuses cloud-metadata (private) and non-http targets
    // before any browser is launched.
    assert!(r[17].error, "{}", r[17].text);
    assert!(!r[17].text.starts_with("Opened."), "{}", r[17].text);
    assert!(r[18].error, "{}", r[18].text);
    assert!(!r[18].text.starts_with("Opened."), "{}", r[18].text);
}
