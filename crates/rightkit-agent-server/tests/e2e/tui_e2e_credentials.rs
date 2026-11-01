//! Tool-origin Cloudflare credential recovery runs through the real TUI
//! process. Provider HTTP is restricted to an isolated loopback fixture.

use crate::support::tui_harness as h;
use h::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

const TOKEN: &str = "e2e-cf-token-9041";

struct CloudflareMock {
    base_url: String,
    authorization: Arc<Mutex<Option<String>>>,
}

impl CloudflareMock {
    fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback Cloudflare mock");
        let port = listener.local_addr().unwrap().port();
        let authorization = Arc::new(Mutex::new(None));
        let received = authorization.clone();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).ok() != Some(1) {
                    return;
                }
                request.push(byte[0]);
            }
            let headers = String::from_utf8_lossy(&request);
            let auth = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_string())
            });
            *received.lock().unwrap() = auth;
            let body = r#"{"success":true,"result":[{"name":"fixture-project"}]}"#;
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/client/v4/"),
            authorization,
        }
    }
}

fn cloudflare_call(id: &str) -> Step {
    Step::tool(
        id,
        "cloudflare",
        json!({"action":"list_pages_projects","account_id":"fixture-account"}),
    )
}

#[test]
fn missing_cloudflare_token_is_masked_then_used_by_the_same_tool_call() {
    let mock = CloudflareMock::start();
    let provider = ScriptedProvider::start(vec![
        cloudflare_call("credential-submit"),
        Step::text("project lookup completed"),
    ]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            args: &["--yolo"],
            env: &[
                ("RIGHTKIT_E2E_LOCAL_PROVIDER_MOCK", "1"),
                ("RIGHTKIT_E2E_CLOUDFLARE_BASE_URL", &mock.base_url),
            ],
            ..Default::default()
        },
    );
    tui.ready();
    tui.submit("list Cloudflare Pages projects");
    let card = tui.wait_viewport("Cloudflare credential card", |view| {
        view.contains("Credential needed") && view.contains("cloudflare")
    });
    assert!(
        card.contains("Enter submit") && card.contains("Esc cancel"),
        "credential actions missing:\n{card}"
    );
    tui.type_text(TOKEN);
    let masked = "•".repeat(TOKEN.chars().count().min(24));
    let view = tui.wait_viewport("masked credential entry", |view| view.contains(&masked));
    assert!(
        !view.contains(TOKEN),
        "credential input was rendered in clear text"
    );
    tui.enter();
    tui.wait_for_text("project lookup completed");
    let expected_authorization = format!("Bearer {TOKEN}");
    assert!(
        mock.authorization.lock().unwrap().as_deref() == Some(expected_authorization.as_str()),
        "mock provider did not receive submitted credential"
    );
    let transcript = tui.screen_text();
    assert!(
        !transcript.contains(TOKEN),
        "credential leaked into TUI transcript"
    );
    assert_eq!(
        provider.request_count(),
        2,
        "the original action resumed and returned to the model"
    );
}

#[test]
fn cancelling_cloudflare_credential_request_continues_turn_without_provider_call() {
    let provider = ScriptedProvider::start(vec![
        cloudflare_call("credential-cancel"),
        Step::text("continuing without Cloudflare access"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("list Cloudflare Pages projects");
    tui.wait_viewport("Cloudflare credential card", |view| {
        view.contains("Credential needed") && view.contains("cloudflare")
    });
    tui.key(ESC);
    tui.wait_for_text("continuing without Cloudflare access");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "cancelling resumes the original turn");
    let tool_output = messages(&requests[1])
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        tool_output.to_ascii_lowercase().contains("declined"),
        "tool receives a typed decline result"
    );
}
